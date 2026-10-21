//! Open tracking: domains, settings, the public image routes, the send hook and the lead erase.
//! These tests need Postgres. They use a fake CNAME resolver, never real DNS, and never a real ACME server.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::tracking::dns::FakeCname;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

struct T {
    h: Harness,
    keys: HashMap<Scope, String>,
    dns: std::sync::Arc<FakeCname>,
}

const HTML: &str = "<p>Hi {{first_name|there}},</p><p>Best,<br><b>Test Sender</b></p>";

impl T {
    async fn new(pool: PgPool) -> Self {
        let mut h = Harness::new(pool);
        let dns = FakeCname::new();
        h.state.cname = dns.clone();
        let keys = bootstrap_keys(&h.state).await;
        Self { h, keys, dns }
    }

    async fn api(&self, scope: Scope, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&scope]), body).await
    }

    async fn campaign(&self, account: Uuid) -> String {
        let body = json!({ "name": "C", "account_ids": [account], "sequence": { "name": "S", "steps": [{ "subject": "Hi", "body_text": "Hi", "body_html": HTML }] } });
        let r = self.api(Scope::Campaigns, "POST", "/v1/campaigns", Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body["id"].as_str().unwrap().to_string()
    }

    async fn check(&self, id: &str) -> Response {
        self.api(Scope::Tracking, "POST", &format!("/v1/tracking/domains/{id}/check"), None).await
    }

    /// Point the CNAME of `media.<domain>` at this server and check.
    async fn connect(&self, domain: &str) {
        let list = self.api(Scope::Tracking, "GET", "/v1/tracking/domains", None).await.body;
        let d = list["domains"].as_array().unwrap().iter().find(|d| d["sending_domain"] == domain).unwrap().clone();
        let target = d["record"]["value"].as_str().unwrap().to_string();
        self.dns.set(&format!("{}.{domain}", d["subdomain"].as_str().unwrap()), &target);
        let r = self.api(Scope::Tracking, "POST", &format!("/v1/tracking/domains/{}/check", d["id"].as_str().unwrap()), None).await;
        assert_eq!(r.body["status"], "connected", "{}", r.body);
    }
}

#[sqlx::test]
async fn domains_follow_the_cname(pool: PgPool) {
    let t = T::new(pool).await;
    t.h.account("nazmul@acme.test").await;
    t.h.account("second@acme.test").await;
    let list = t.api(Scope::Tracking, "GET", "/v1/tracking/domains", None).await.body;
    let rows = list["domains"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one row for each sending domain");
    let d = &rows[0];
    assert_eq!((d["sending_domain"].as_str(), d["subdomain"].as_str(), d["host"].as_str()), (Some("acme.test"), Some("media"), Some("media.acme.test")));
    assert_eq!(d["status"], "not_connected");
    assert_eq!((d["record"]["type"].as_str(), d["record"]["name"].as_str()), (Some("CNAME"), Some("media")));
    let id = d["id"].as_str().unwrap().to_string();

    // no CNAME: still not connected, and it says why
    let r = t.check(&id).await;
    assert_eq!(r.body["status"], "not_connected");
    assert!(r.body["error_message"].as_str().unwrap().contains("No CNAME"));
    // a CNAME to the wrong place
    t.dns.set("media.acme.test", "other.example.com");
    assert_eq!(t.check(&id).await.body["status"], "not_connected");
    // the right CNAME (no HTTPS listener in the test config: connected at once)
    t.connect("acme.test").await;
    // removed later: red
    t.dns.remove("media.acme.test");
    let r = t.check(&id).await;
    assert_eq!(r.body["status"], "error");
    assert!(r.body["error_message"].as_str().unwrap().contains("removed"));
    // a DNS timeout changes nothing
    t.connect("acme.test").await;
    t.dns.fail("media.acme.test");
    assert_eq!(t.check(&id).await.body["status"], "connected");
}

#[sqlx::test]
async fn the_subdomain_can_change_but_not_to_a_blocked_name(pool: PgPool) {
    let t = T::new(pool).await;
    t.h.account("a@acme.test").await;
    t.connect("acme.test").await;
    let id = t.api(Scope::Tracking, "GET", "/v1/tracking/domains", None).await.body["domains"][0]["id"].as_str().unwrap().to_string();
    for bad in ["tracking", "click", "pixel", "open", "track"] {
        let r = t.api(Scope::Tracking, "PUT", &format!("/v1/tracking/domains/{id}"), Some(json!({ "subdomain": bad }))).await;
        assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_subdomain")), "{bad}");
    }
    let r = t.api(Scope::Tracking, "PUT", &format!("/v1/tracking/domains/{id}"), Some(json!({ "subdomain": "img" }))).await;
    assert_eq!((r.body["host"].as_str(), r.body["status"].as_str()), (Some("img.acme.test"), Some("not_connected")), "a new name starts over");
}

#[sqlx::test]
async fn trackers_unlock_only_when_every_sending_domain_is_connected(pool: PgPool) {
    let t = T::new(pool).await;
    let a = t.h.account("a@acme.test").await;
    let b = t.h.account("b@other.test").await;
    let body = json!({ "name": "C", "account_ids": [a, b], "sequence": { "name": "S", "steps": [{ "subject": "Hi", "body_text": "Hi" }] } });
    let camp = t.api(Scope::Campaigns, "POST", "/v1/campaigns", Some(body)).await.body["id"].as_str().unwrap().to_string();
    let uri = format!("/v1/campaigns/{camp}/tracking");
    let off = t.api(Scope::Tracking, "GET", &uri, None).await.body;
    assert_eq!((off["enabled"].clone(), off["trackers_unlocked"].clone()), (json!(false), json!(false)));
    assert_eq!(off["domains"].as_array().unwrap().len(), 2);

    let want = json!({ "enabled": true, "tracker_type": "text_image", "text_value": "Test Sender" });
    let r = t.api(Scope::Tracking, "PUT", &uri, Some(want.clone())).await;
    assert_eq!(r.body["error"]["code"], "tracking_domains_not_ready", "{}", r.body);
    t.connect("acme.test").await;
    assert_eq!(t.api(Scope::Tracking, "PUT", &uri, Some(want.clone())).await.status, StatusCode::UNPROCESSABLE_ENTITY, "one domain is still red");
    t.connect("other.test").await;
    let r = t.api(Scope::Tracking, "PUT", &uri, Some(want)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!((r.body["enabled"].clone(), r.body["trackers_unlocked"].clone()), (json!(true), json!(true)));
    // turning it off needs nothing
    assert_eq!(t.api(Scope::Tracking, "PUT", &uri, Some(json!({ "enabled": false }))).await.body["enabled"], false);
    // the scope guards the routes
    assert_eq!(call(&t.h.state, "GET", &uri, Some(&t.keys[&Scope::Leads]), None).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn the_preview_draws_the_text_in_the_style_of_the_first_email(pool: PgPool) {
    let t = T::new(pool).await;
    let a = t.h.account("a@acme.test").await;
    let camp = t.campaign(a).await;
    let r = t
        .api(Scope::Tracking, "POST", &format!("/v1/campaigns/{camp}/tracking/preview"), Some(json!({ "tracker_type": "text_image", "text_value": "Test Sender" })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["text_found"], true);
    assert_eq!(r.body["style_source"], "email");
    assert_eq!(r.body["style"]["bold"], true, "the name is inside <b>");
    assert!(r.body["image"].as_str().unwrap().starts_with("data:image/png;base64,"));
    let none = t.api(Scope::Tracking, "POST", &format!("/v1/campaigns/{camp}/tracking/preview"), Some(json!({ "tracker_type": "text_image", "text_value": "Nobody" }))).await;
    assert_eq!((none.body["text_found"].clone(), none.body["style_source"].clone()), (json!(false), json!("default")));
}

#[sqlx::test]
async fn the_image_routes_always_answer_with_an_image_and_log_opens(pool: PgPool) {
    let t = T::new(pool).await;
    let a = t.h.account("a@acme.test").await;
    let job = t.h.queue(a, "lead@x.test").await;
    let asset = Uuid::now_v7();
    let png = aikido_email::tracking::render::render_text("Test Sender", &Default::default()).unwrap().png;
    sqlx::query("INSERT INTO tracking_assets (id, kind, cache_key, file, content_type) VALUES ($1, 'text_image', 'k', $2, 'image/png')").bind(asset).bind(&png).execute(&t.h.state.pool).await.unwrap();
    let token = "aB3dE6gH9jK2mN5pQ8sT1u";
    sqlx::query("INSERT INTO tracking_tokens (token, send_job_id, asset_id) VALUES ($1, $2, $3)").bind(token).bind(job).bind(asset).execute(&t.h.state.pool).await.unwrap();
    // the send was a long time ago, so only a proxy is marked
    sqlx::query("UPDATE send_jobs SET sent_at = now() - interval '2 hours' WHERE id = $1").bind(job).execute(&t.h.state.pool).await.unwrap();

    let get = |uri: String, ua: &'static str| {
        let state = t.h.state.clone();
        async move {
            let app = aikido_email::docs::app(state);
            let req = axum::http::Request::builder().uri(uri).header("user-agent", ua).body(axum::body::Body::empty()).unwrap();
            let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
            let (parts, body) = resp.into_parts();
            (parts, axum::body::to_bytes(body, 1 << 20).await.unwrap())
        }
    };
    // unknown token: an image, never an error
    let (p, body) = get("/i/AAAAAAAAAAAAAAAAAAAAAA.png".into(), "Mozilla").await;
    assert_eq!(p.status, StatusCode::OK);
    assert_eq!(p.headers["content-type"], "image/png");
    assert_eq!(p.headers["cache-control"], "no-cache, no-store, must-revalidate");
    assert!(body.starts_with(b"\x89PNG"));
    let (p, _) = get("/p/not-a-token".into(), "Mozilla").await;
    assert_eq!(p.status, StatusCode::OK);

    // the known token: the stored image, and two opens
    let (p, body) = get(format!("/i/{token}.png"), "Mozilla/5.0 (Macintosh)").await;
    assert_eq!((p.status, body.to_vec()), (StatusCode::OK, png.clone()));
    assert_eq!(p.headers["content-length"], png.len().to_string());
    get(format!("/i/{token}.png"), "Mozilla/5.0 (via ggpht.com GoogleImageProxy)").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let rows: Vec<(bool, Option<String>)> = sqlx::query_as("SELECT is_proxy, proxy_kind FROM email_opens WHERE token = $1 ORDER BY id").bind(token).fetch_all(&t.h.state.pool).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.contains(&(false, None)));
    assert!(rows.contains(&(true, Some("google".into()))));
}

#[sqlx::test]
async fn the_send_hook_puts_the_tracker_in_the_final_html_and_erase_removes_it(pool: PgPool) {
    let t = T::new(pool).await;
    let a = t.h.account("nazmul@acme.test").await;
    let camp = t.campaign(a).await;
    t.connect("acme.test").await;
    let r = t.api(Scope::Tracking, "PUT", &format!("/v1/campaigns/{camp}/tracking"), Some(json!({ "enabled": true, "tracker_type": "text_image", "text_value": "Nazmul Hasan" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let lead = t.api(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": "lead@x.test" }))).await.body["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let job_id = t.h.queue(a, "lead@x.test").await;
    sqlx::query("UPDATE send_jobs SET campaign_id = $2, lead_id = $3 WHERE id = $1").bind(job_id).bind(camp.parse::<Uuid>().unwrap()).bind(lead).execute(&t.h.state.pool).await.unwrap();
    let job = t.h.job(job_id).await;
    let account = t.h.account_row(a).await;

    let html = "<p>Hi,</p><p>Best,<br>Nazmul <b>Hasan</b></p>";
    let out = aikido_email::tracking::send::apply(&t.h.state, &job, &account, html).await.expect("a tracker is inserted");
    assert!(out.contains("<img src=\"https://media.acme.test/i/") && out.contains("alt=\"Nazmul Hasan\""), "{out}");
    assert!(!out.contains("Hasan</b>"), "the text is replaced: {out}");
    let token: String = sqlx::query_scalar("SELECT tracking_token FROM send_jobs WHERE id = $1").bind(job_id).fetch_one(&t.h.state.pool).await.unwrap();
    assert!(out.contains(&token));
    // a retry of the same job keeps its token
    let again = aikido_email::tracking::send::apply(&t.h.state, &job, &account, html).await.unwrap();
    assert!(again.contains(&token));
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM tracking_tokens").await, 1);
    // text not in the email: no tracker, and a warning on the campaign
    assert!(aikido_email::tracking::send::apply(&t.h.state, &job, &account, "<p>Hello</p>").await.is_none());
    let s = t.api(Scope::Tracking, "GET", &format!("/v1/campaigns/{camp}/tracking"), None).await.body;
    assert!(s["warning"].as_str().unwrap().contains("not in the email"), "{s}");
    // a domain that turned red: no tracker
    t.dns.remove("media.acme.test");
    let id = t.api(Scope::Tracking, "GET", "/v1/tracking/domains", None).await.body["domains"][0]["id"].as_str().unwrap().to_string();
    t.api(Scope::Tracking, "POST", &format!("/v1/tracking/domains/{id}/check"), None).await;
    assert!(aikido_email::tracking::send::apply(&t.h.state, &job, &account, html).await.is_none());

    // an open, then the lead is erased: tokens and opens go
    aikido_email::tracking::server::record_open(&t.h.state, &token, job_id, Some(camp.parse().unwrap()), Some(lead), None, "203.0.113.9", "Mozilla").await;
    let o = t.api(Scope::Tracking, "GET", &format!("/v1/leads/{lead}/opens"), None).await.body;
    assert_eq!(o["summary"]["status"], "maybe_opened", "the send is not saved yet: a scanner");
    let ctx = t.api(Scope::Leads, "GET", &format!("/v1/leads/{lead}/context"), None).await.body;
    assert_eq!(ctx["opens"]["status"], "maybe_opened");
    let r = t.api(Scope::Leads, "DELETE", &format!("/v1/leads/{lead}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM email_opens").await, 0);
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM tracking_tokens").await, 0);
}

#[sqlx::test]
async fn campaign_numbers_count_opened_maybe_opened_and_repeat_opens(pool: PgPool) {
    let t = T::new(pool).await;
    let a = t.h.account("a@acme.test").await;
    let camp = t.campaign(a).await;
    let camp_id: Uuid = camp.parse().unwrap();
    let asset = Uuid::now_v7();
    sqlx::query("INSERT INTO tracking_assets (id, kind, cache_key, file, content_type) VALUES ($1, 'text_image', 'k', 'x', 'image/png')").bind(asset).execute(&t.h.state.pool).await.unwrap();
    let mut leads = Vec::new();
    for n in 0..3 {
        let lead = t.api(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": format!("l{n}@x.test") }))).await.body["id"].as_str().unwrap().parse::<Uuid>().unwrap();
        let job = t.h.queue(a, &format!("l{n}@x.test")).await;
        sqlx::query("UPDATE send_jobs SET status = 'sent', sent_at = now() - interval '1 day', campaign_id = $2, lead_id = $3 WHERE id = $1").bind(job).bind(camp_id).bind(lead).execute(&t.h.state.pool).await.unwrap();
        sqlx::query("INSERT INTO tracking_tokens (token, send_job_id, campaign_id, lead_id, asset_id) VALUES ($1, $2, $3, $4, $5)")
            .bind(format!("tok{n:019}"))
            .bind(job)
            .bind(camp_id)
            .bind(lead)
            .bind(asset)
            .execute(&t.h.state.pool)
            .await
            .unwrap();
        leads.push((lead, job));
    }
    let open = |n: usize, proxy: bool| {
        let (lead, job) = leads[n];
        let pool = t.h.state.pool.clone();
        let camp_id = camp_id;
        async move {
            sqlx::query("INSERT INTO email_opens (token, send_job_id, lead_id, campaign_id, is_proxy) VALUES ('t', $1, $2, $3, $4)").bind(job).bind(lead).bind(camp_id).bind(proxy).execute(&pool).await.unwrap();
        }
    };
    open(0, false).await;
    open(0, false).await;
    open(0, false).await; // lead 0: opened, 2 repeat opens
    open(1, true).await; // lead 1: only a proxy: maybe opened
    let r = t.api(Scope::Tracking, "GET", &format!("/v1/campaigns/{camp}/opens"), None).await.body;
    assert_eq!((r["tracked_leads"].clone(), r["opened"].clone(), r["maybe_opened"].clone(), r["repeat_opens"].clone()), (json!(3), json!(1), json!(1), json!(2)), "{r}");
    assert!((r["open_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
    assert_eq!(r["recent"].as_array().unwrap().len(), 2);
    // the lead tables show it
    let rows = t.api(Scope::Campaigns, "GET", &format!("/v1/campaigns/{camp}/leads"), None).await.body;
    let status_of = |lead: Uuid| rows["leads"].as_array().unwrap().iter().find(|l| l["lead_id"] == lead.to_string()).unwrap()["open_status"].clone();
    assert_eq!((status_of(leads[0].0), status_of(leads[1].0)), (json!("opened"), json!("maybe_opened")));
}
