//! Lead import (CSV), the import worker, and the CSV export.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::leads::import::{self, LEASE_SECS, RunOpts};
use aikido_email::leads::verify::{MxAnswer, fake::FakeMx};
use axum::http::StatusCode;
use chrono::Duration;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

struct I {
    h: Harness,
    keys: HashMap<Scope, String>,
    mx: FakeMx,
}

impl I {
    async fn new(pool: PgPool) -> Self {
        let mut h = Harness::new(pool);
        let mx = FakeMx::new(MxAnswer::Has);
        h.state.mx = Arc::new(mx.clone());
        let keys = bootstrap_keys(&h.state).await;
        Self { h, keys, mx }
    }

    fn key(&self) -> &str {
        &self.keys[&Scope::Leads]
    }

    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(self.key()), body).await
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = self.api(method, uri, body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    /// Start an import from CSV text. `extra` are form fields.
    async fn start_raw(&self, csv: &str, extra: &[(&str, &str)]) -> Response {
        let mut parts: Vec<(&str, Option<&str>, &[u8])> = vec![("file", Some("leads.csv"), csv.as_bytes())];
        for (k, v) in extra {
            parts.push((k, None, v.as_bytes()));
        }
        call_form(&self.h.state, "/v1/leads/imports", Some(self.key()), &parts).await
    }

    async fn start(&self, csv: &str, extra: &[(&str, &str)]) -> Uuid {
        let r = self.start_raw(csv, extra).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body["id"].as_str().unwrap().parse().unwrap()
    }

    async fn tick(&self) -> usize {
        import::tick(&self.h.state, &RunOpts::default()).await.unwrap()
    }

    async fn import(&self, id: Uuid) -> Value {
        self.ok("GET", &format!("/v1/leads/imports/{id}"), None).await
    }

    async fn lead(&self, email: &str) -> Value {
        let r = self.ok("GET", &format!("/v1/leads?q={}&limit=1", email.replace('@', "%40")), None).await;
        r["leads"][0].clone()
    }

    async fn count(&self) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM leads").fetch_one(&self.h.state.pool).await.unwrap().0
    }
}

fn counters(i: &Value) -> (i64, i64, i64, i64) {
    (i["created"].as_i64().unwrap(), i["updated"].as_i64().unwrap(), i["skipped"].as_i64().unwrap(), i["invalid"].as_i64().unwrap())
}

// ---------- preview and mapping ----------

#[sqlx::test]
async fn preview_guesses_the_mapping(pool: PgPool) {
    let i = I::new(pool).await;
    i.ok("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    // BOM, semicolons, odd column names
    let csv = "\u{feff}E-mail Address;First Name;LastName;Organization;Pain Point;Junk;Company\nA@x.com;Ana;Ng;Acme;slow;1;Second\n";
    let r = call_form(&i.h.state, "/v1/leads/imports/preview", Some(i.key()), &[("file", Some("a.csv"), csv.as_bytes())]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["delimiter"], ";");
    assert_eq!(r.body["email_column"], "E-mail Address");
    assert_eq!(
        r.body["mapping"],
        json!({ "E-mail Address": "email", "First Name": "first_name", "LastName": "last_name", "Organization": "company_name", "Pain Point": "custom:pain_point", "Junk": "ignore", "Company": "ignore" })
    );
    assert_eq!(r.body["sample_rows"][0][0], "A@x.com");
    assert_eq!(i.count().await, 0, "preview saves nothing");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_imports").fetch_one(&i.h.state.pool).await.unwrap();
    assert_eq!(n, 0);
    // only 20 rows
    let big: String = std::iter::once("email\n".to_string()).chain((0..50).map(|n| format!("u{n}@x.com\n"))).collect();
    let r = call_form(&i.h.state, "/v1/leads/imports/preview", Some(i.key()), &[("file", Some("a.csv"), big.as_bytes())]).await;
    assert_eq!(r.body["sample_rows"].as_array().unwrap().len(), 20);
    let r = call_form(&i.h.state, "/v1/leads/imports/preview", Some(i.key()), &[]).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("file_required")));
}

#[sqlx::test]
async fn start_checks_before_it_stores_anything(pool: PgPool) {
    let i = I::new(pool).await;
    let code = |r: &Response| r.body["error"]["code"].as_str().unwrap_or("").to_string();
    let r = i.start_raw("name,company\nAna,Acme\n", &[]).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "no_email_column"));
    let r = i.start_raw("email\n", &[]).await;
    assert_eq!(code(&r), "empty_file");
    let r = i.start_raw("email,name\na@x.com,Ana\n", &[("mapping", "{\"Nope\":\"email\"}")]).await;
    assert_eq!(code(&r), "unknown_column");
    let r = i.start_raw("email,name\na@x.com,Ana\n", &[("mapping", "{\"email\":\"email\",\"name\":\"email\"}")]).await;
    assert_eq!(code(&r), "duplicate_target");
    let r = i.start_raw("email,name\na@x.com,Ana\n", &[("mapping", "{\"email\":\"email\",\"name\":\"bogus\"}")]).await;
    assert_eq!(code(&r), "invalid_mapping");
    let r = i.start_raw("email,name\na@x.com,Ana\n", &[("mapping", "not json")]).await;
    assert_eq!(code(&r), "invalid_mapping");
    let r = i.start_raw("email,pain\na@x.com,slow\n", &[("mapping", "{\"email\":\"email\",\"pain\":\"custom:pain_point\"}")]).await;
    assert_eq!(code(&r), "unknown_field");
    let r = i.start_raw("email\na@x.com\n", &[("dedupe_mode", "merge")]).await;
    assert_eq!(code(&r), "invalid_dedupe_mode");
    let r = i.start_raw("email\na@x.com\n", &[("list_id", &Uuid::now_v7().to_string())]).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::NOT_FOUND, "list_not_found"));
    let r = i.start_raw("email\na@x.com\n", &[("tags", &"x".repeat(60))]).await;
    assert_eq!(code(&r), "invalid_tags");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_imports").fetch_one(&i.h.state.pool).await.unwrap();
    assert_eq!(n, 0, "a refused import leaves nothing behind");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_fields").fetch_one(&i.h.state.pool).await.unwrap();
    assert_eq!(n, 0);

    // create_fields makes the field
    let id = i.start("email,pain\na@x.com,slow\n", &[("mapping", "{\"email\":\"email\",\"pain\":\"custom:pain_point\"}"), ("create_fields", "true")]).await;
    assert_eq!(i.ok("GET", "/v1/leads/fields", None).await[0]["key"], "pain_point");
    i.tick().await;
    assert_eq!(i.import(id).await["status"], "done");
    assert_eq!(i.lead("a@x.com").await["custom_fields"], json!({ "pain_point": "slow" }));
}

#[sqlx::test]
async fn row_and_size_limits_come_from_the_settings(pool: PgPool) {
    let i = I::new(pool).await;
    i.ok("PATCH", "/v1/leads/settings", Some(json!({ "import_max_rows": 3, "import_max_bytes": 1024 }))).await;
    let r = i.start_raw("email\na@x.com\nb@x.com\nc@x.com\nd@x.com\n", &[]).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("too_many_rows")));
    assert_eq!(i.start_raw("email\na@x.com\nb@x.com\nc@x.com\n", &[]).await.status, StatusCode::CREATED);
    let big = format!("email\n{}\n", "a".repeat(2000));
    let r = i.start_raw(&big, &[]).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::PAYLOAD_TOO_LARGE, Some("file_too_large")));
}

// ---------- a normal run ----------

#[sqlx::test]
async fn import_writes_leads_counts_and_reports_bad_rows(pool: PgPool) {
    let i = I::new(pool).await;
    let list = i.ok("POST", "/v1/leads/lists", Some(json!({ "name": "March" }))).await["id"].as_str().unwrap().to_string();
    let long = "x".repeat(1001);
    let csv = format!(
        "Email,First Name,Company,Tags,Timezone\n\
         \"  Ana@Acme.COM \",Ana,Acme,\"a; b\",Asia/Dhaka\n\
         ben@acme.com,Ben,,,\n\
         not-an-email,Carl,Acme,,\n\
         ANA@acme.com,Duplicate,Other,,\n\
         dina@acme.com,{long},Acme,,\n\
         eli@acme.com,Eli,Acme,,Mars/Base\n\
         ,,,,\n\
         fay@acme.com,Fay,Acme,,\n"
    );
    let id = i.start(&csv, &[("list_id", &list), ("tags", "Batch1, March")]).await;
    let before = i.import(id).await;
    assert_eq!((before["status"].as_str(), before["total_rows"].as_i64()), (Some("pending"), Some(8)));
    assert_eq!(i.tick().await, 1);
    let r = i.import(id).await;
    assert_eq!(r["status"], "done", "{r}");
    assert_eq!(counters(&r), (3, 0, 1, 4), "{r}");
    assert_eq!(r["rows_done"], 8);
    assert!(r["finished_at"].is_string());
    let rows: Vec<i64> = r["errors"].as_array().unwrap().iter().map(|e| e["row"].as_i64().unwrap()).collect();
    assert_eq!(rows, [4, 6, 7, 8], "the line of each bad row in the file: {r}");
    assert!(r["errors"][0]["reason"].as_str().unwrap().contains('@'));
    assert!(r["errors"][3]["reason"].as_str().unwrap().contains("email"));

    assert_eq!(i.count().await, 3);
    let ana = i.lead("ana@acme.com").await;
    assert_eq!((ana["first_name"].as_str(), ana["company_name"].as_str()), (Some("Ana"), Some("Acme")), "the first row of an email wins");
    assert_eq!(ana["tags"], json!(["a", "b", "batch1", "march"]));
    assert_eq!(ana["source"], "csv");
    assert_eq!(ana["source_ref"], id.to_string());
    assert_eq!(ana["timezone"], "Asia/Dhaka");
    assert_eq!(ana["verification"], "mx_ok");
    let in_list = i.ok("GET", &format!("/v1/leads?list_id={list}&limit=500"), None).await;
    assert_eq!(in_list["leads"].as_array().unwrap().len(), 3);
    // the file is gone
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_import_files").fetch_one(&i.h.state.pool).await.unwrap();
    assert_eq!(n, 0);
    let events = i.ok("GET", &format!("/v1/leads/{}/events", ana["id"].as_str().unwrap()), None).await;
    let created = events["events"].as_array().unwrap().iter().find(|e| e["kind"] == "created").expect("a created event");
    assert_eq!(created["ref_id"], id.to_string());
    assert_eq!(created["detail"]["source"], "csv");
    // listed
    let list_r = i.ok("GET", "/v1/leads/imports", None).await;
    assert_eq!(list_r["imports"][0]["id"], id.to_string());
    assert_eq!(list_r["imports"][0]["errors"], json!([]), "the list leaves the bad rows out");
    assert_eq!(i.ok("GET", "/v1/leads/imports?status=pending", None).await["imports"], json!([]));
    assert_eq!(i.api("GET", &format!("/v1/leads/imports/{}", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn suppressed_emails_are_imported_and_flagged(pool: PgPool) {
    let i = I::new(pool).await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('blocked@x.com', 'email', 'manual')").execute(&i.h.state.pool).await.unwrap();
    i.start("email\nblocked@x.com\nfree@x.com\n", &[]).await;
    i.tick().await;
    assert_eq!(i.count().await, 2);
    assert_eq!(i.lead("blocked@x.com").await["id"].is_string(), true);
    let r = i.ok("GET", "/v1/leads?suppressed=true", None).await;
    assert_eq!(r["leads"][0]["email"], "blocked@x.com");
}

#[sqlx::test]
async fn delimiters_and_odd_files(pool: PgPool) {
    let i = I::new(pool).await;
    // tab separated, short rows, long rows
    i.start("email\tfirst_name\nw@x.com\tWill\nshort@x.com\nlong@x.com\tLo\textra\textra\n", &[]).await;
    i.tick().await;
    assert_eq!(i.count().await, 3);
    assert_eq!(i.lead("w@x.com").await["first_name"], "Will");
    assert_eq!(i.lead("short@x.com").await["first_name"], Value::Null);
    // same header twice: the first column is used
    i.start("email,name,name\nd@x.com,First,Second\n", &[("mapping", "{\"email\":\"email\",\"name\":\"first_name\"}")]).await;
    i.tick().await;
    assert_eq!(i.lead("d@x.com").await["first_name"], "First");
}

// ---------- duplicate modes ----------

async fn dupe_setup(i: &I) {
    i.ok("POST", "/v1/leads", Some(json!({ "email": "old@x.com", "first_name": "Old", "job_title": "CEO", "tags": ["keep"] }))).await;
}
const DUPE_CSV: &str = "email,first_name,last_name,job_title,company\nold@x.com,New,Last,,Acme\nnew@x.com,Fresh,,,\n";

#[sqlx::test]
async fn mode_skip(pool: PgPool) {
    let i = I::new(pool).await;
    dupe_setup(&i).await;
    let list = i.ok("POST", "/v1/leads/lists", Some(json!({ "name": "L" }))).await["id"].as_str().unwrap().to_string();
    let id = i.start(DUPE_CSV, &[("dedupe_mode", "skip"), ("list_id", &list), ("tags", "t1")]).await;
    i.tick().await;
    assert_eq!(counters(&i.import(id).await), (1, 0, 1, 0));
    let old = i.lead("old@x.com").await;
    assert_eq!((old["first_name"].as_str(), old["last_name"].as_str(), old["company_name"].as_str()), (Some("Old"), None, None), "untouched");
    // still added to the list and tagged
    assert_eq!(old["tags"], json!(["keep", "t1"]));
    assert_eq!(i.ok("GET", &format!("/v1/leads?list_id={list}"), None).await["leads"].as_array().unwrap().len(), 2);
}

#[sqlx::test]
async fn mode_fill_empty_is_the_default(pool: PgPool) {
    let i = I::new(pool).await;
    dupe_setup(&i).await;
    let id = i.start(DUPE_CSV, &[]).await;
    assert_eq!(i.import(id).await["dedupe_mode"], "fill_empty");
    i.tick().await;
    assert_eq!(counters(&i.import(id).await), (1, 1, 0, 0));
    let old = i.lead("old@x.com").await;
    assert_eq!((old["first_name"].as_str(), old["last_name"].as_str(), old["job_title"].as_str(), old["company_name"].as_str()), (Some("Old"), Some("Last"), Some("CEO"), Some("Acme")));
    // the same file again changes nothing: both leads count as skipped
    let id2 = i.start(DUPE_CSV, &[]).await;
    i.tick().await;
    assert_eq!(counters(&i.import(id2).await), (0, 0, 2, 0));
}

#[sqlx::test]
async fn mode_overwrite_never_erases_with_an_empty_cell(pool: PgPool) {
    let i = I::new(pool).await;
    dupe_setup(&i).await;
    let id = i.start(DUPE_CSV, &[("dedupe_mode", "overwrite")]).await;
    i.tick().await;
    assert_eq!(counters(&i.import(id).await), (1, 1, 0, 0));
    let old = i.lead("old@x.com").await;
    assert_eq!((old["first_name"].as_str(), old["last_name"].as_str(), old["job_title"].as_str()), (Some("New"), Some("Last"), Some("CEO")), "the empty job_title cell did not erase CEO");
    assert_eq!(old["tags"], json!(["keep"]));
}

#[sqlx::test]
async fn repeat_in_file_overwrite(pool: PgPool) {
    let i = I::new(pool).await;
    i.start("email,first_name\na@x.com,First\na@x.com,Second\nA@X.com,Third\n", &[("dedupe_mode", "overwrite")]).await;
    i.tick().await;
    assert_eq!(i.lead("a@x.com").await["first_name"], "First");
    let r = i.ok("GET", "/v1/leads/imports", None).await;
    assert_eq!(counters(&r["imports"][0]), (1, 0, 2, 0));
}

// ---------- crash, lease, two workers, cancel ----------

fn big_csv(n: usize, domains: usize) -> String {
    let mut s = String::from("email,first_name,company\n");
    for k in 0..n {
        s.push_str(&format!("user{k}@company{}.com,Name{k},Company {}\n", k % domains, k % domains));
    }
    s
}

#[sqlx::test]
async fn killed_worker_resumes_with_the_right_counts(pool: PgPool) {
    let i = I::new(pool).await;
    // 2500 rows; row 1501 repeats row 11 (written before the crash); row 1502 is bad
    let mut csv = big_csv(2497, 40);
    csv.push_str("user10@company10.com,Repeat,Other\n");
    csv.push_str("bad email,x,y\n");
    csv.push_str("last@end.com,Last,End\n");
    let id = i.start(&csv, &[("dedupe_mode", "overwrite")]).await;

    // the worker dies after the first batch
    let ended = import::tick(&i.h.state, &RunOpts { max_batches: Some(1) }).await.unwrap();
    assert_eq!(ended, 0);
    let mid = i.import(id).await;
    assert_eq!((mid["status"].as_str(), mid["rows_done"].as_i64()), (Some("running"), Some(1000)));
    assert_eq!(counters(&mid), (1000, 0, 0, 0), "live progress");
    assert_eq!(i.count().await, 1000);

    // the lease has not run out: nobody else takes it
    assert_eq!(i.tick().await, 0);
    assert_eq!(i.import(id).await["rows_done"], 1000);

    // the lease runs out. Another worker resumes.
    i.h.advance(Duration::seconds(LEASE_SECS + 1));
    assert_eq!(i.tick().await, 1);
    let r = i.import(id).await;
    assert_eq!(r["status"], "done", "{r}");
    assert_eq!(counters(&r), (2498, 0, 1, 1), "{r}");
    assert_eq!(r["rows_done"], 2500);
    assert_eq!(i.count().await, 2498);
    assert_eq!(i.lead("user10@company10.com").await["first_name"], "Name10", "the repeat after the crash did not overwrite the first row");
    assert_eq!(r["errors"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn two_workers_at_the_same_time_write_each_row_once(pool: PgPool) {
    let i = I::new(pool).await;
    let id = i.start(&big_csv(3200, 60), &[]).await;
    let opts = RunOpts::default();
    let (a, b) = tokio::join!(import::tick(&i.h.state, &opts), import::tick(&i.h.state, &opts));
    assert_eq!(a.unwrap() + b.unwrap(), 1, "one import, ended once");
    let r = i.import(id).await;
    assert_eq!((r["status"].as_str(), counters(&r)), (Some("done"), (3200, 0, 0, 0)));
    assert_eq!(i.count().await, 3200);
}

#[sqlx::test]
async fn a_slow_worker_that_lost_its_lease_stops_without_double_counting(pool: PgPool) {
    let i = I::new(pool).await;
    let id = i.start(&big_csv(2500, 10), &[]).await;
    import::tick(&i.h.state, &RunOpts { max_batches: Some(1) }).await.unwrap();
    i.h.advance(Duration::seconds(LEASE_SECS + 1));
    // a second worker takes over and finishes
    assert_eq!(i.tick().await, 1);
    // a stale worker with the old claim token tries to save: its update is refused
    let r = i.import(id).await;
    assert_eq!(counters(&r), (2500, 0, 0, 0));
    let n = sqlx::query("UPDATE lead_imports SET created = created + 1000 WHERE id = $1 AND claim_id = $2").bind(id).bind(Uuid::now_v7()).execute(&i.h.state.pool).await.unwrap().rows_affected();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn cancel_pending_and_running(pool: PgPool) {
    let i = I::new(pool).await;
    let id = i.start("email\na@x.com\n", &[]).await;
    let r = i.ok("POST", &format!("/v1/leads/imports/{id}/cancel"), None).await;
    assert_eq!(r["status"], "cancelled");
    assert_eq!(i.tick().await, 0);
    assert_eq!(i.count().await, 0);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_import_files").fetch_one(&i.h.state.pool).await.unwrap();
    assert_eq!(n, 0);
    assert_eq!(i.api("POST", &format!("/v1/leads/imports/{id}/cancel"), None).await.body["error"]["code"], "import_ended");
    assert_eq!(i.api("POST", &format!("/v1/leads/imports/{}/cancel", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);

    // running: rows already written stay, the rest is never written
    let id = i.start(&big_csv(2500, 10), &[]).await;
    import::tick(&i.h.state, &RunOpts { max_batches: Some(1) }).await.unwrap();
    assert_eq!(i.count().await, 1000);
    assert_eq!(i.ok("POST", &format!("/v1/leads/imports/{id}/cancel"), None).await["status"], "cancelled");
    i.h.advance(Duration::seconds(LEASE_SECS + 1));
    assert_eq!(i.tick().await, 0);
    assert_eq!(i.count().await, 1000);
    let r = i.import(id).await;
    assert_eq!((r["status"].as_str(), counters(&r)), (Some("cancelled"), (1000, 0, 0, 0)));
    assert_eq!(i.api("POST", &format!("/v1/leads/imports/{id}/cancel"), None).await.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn an_import_that_keeps_failing_is_failed_after_5_tries(pool: PgPool) {
    let i = I::new(pool).await;
    let id = i.start(&big_csv(6000, 10), &[]).await;
    for _ in 0..5 {
        import::tick(&i.h.state, &RunOpts { max_batches: Some(1) }).await.unwrap();
        i.h.advance(Duration::seconds(LEASE_SECS + 1));
    }
    assert_eq!(i.tick().await, 0);
    let r = i.import(id).await;
    assert_eq!(r["status"], "failed");
    assert!(r["error"].as_str().unwrap().contains("5 times"));
}

#[sqlx::test]
async fn import_checks_mx_once_per_domain_and_keeps_timeouts_unverified(pool: PgPool) {
    let i = I::new(pool).await;
    i.mx.set("dead.test", MxAnswer::No);
    i.mx.set("slow.test", MxAnswer::Unknown);
    let mut csv = String::from("email\n");
    for k in 0..1500 {
        csv.push_str(&format!("u{k}@busy.test\n"));
    }
    csv.push_str("a@dead.test\nb@slow.test\n");
    i.start(&csv, &[]).await;
    i.tick().await;
    let calls = i.mx.calls();
    assert_eq!(calls.iter().filter(|d| *d == "busy.test").count(), 1, "1500 leads in two batches, one lookup");
    assert_eq!(i.lead("a@dead.test").await["verification"], "no_mx");
    assert_eq!(i.lead("b@slow.test").await["verification"], "syntax_ok");
    assert_eq!(i.lead("u7@busy.test").await["verification"], "mx_ok");
}

#[sqlx::test]
async fn import_100000_rows_in_under_2_minutes(pool: PgPool) {
    let i = I::new(pool).await;
    i.ok("PATCH", "/v1/leads/settings", Some(json!({ "import_max_rows": 100_000 }))).await;
    let csv = big_csv(100_000, 2000);
    let id = i.start(&csv, &[]).await;
    let started = std::time::Instant::now();
    assert_eq!(i.tick().await, 1);
    let took = started.elapsed();
    let r = i.import(id).await;
    assert_eq!((r["status"].as_str(), counters(&r)), (Some("done"), (100_000, 0, 0, 0)));
    assert!(took < std::time::Duration::from_secs(120), "100000 rows took {took:?}");
    eprintln!("100000 rows imported in {took:?}");
    assert_eq!(i.mx.calls().len(), 2000);
}

// ---------- export ----------

#[sqlx::test]
async fn export_csv_escapes_formulas_and_applies_filters(pool: PgPool) {
    let i = I::new(pool).await;
    i.ok("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    i.ok("POST", "/v1/leads", Some(json!({ "email": "a@x.com", "first_name": "=cmd|' /C calc'!A0", "phone": "+8801700000000", "company_name": "Acme, Inc \"Q\"",
        "tags": ["x", "y"], "custom_fields": { "pain_point": "@SUM(A1)" } }))).await;
    i.ok("POST", "/v1/leads", Some(json!({ "email": "b@x.com", "first_name": "Bob", "notes": "line1\nline2" }))).await;
    let (status, headers, body) = call_raw(&i.h.state, "/v1/leads/export.csv", Some(i.key())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
    assert!(headers["content-disposition"].to_str().unwrap().contains("attachment"));
    let mut rd = csv::Reader::from_reader(&body[..]);
    let head: Vec<String> = rd.headers().unwrap().iter().map(str::to_string).collect();
    assert_eq!(head[0], "email");
    assert_eq!(head.last().unwrap(), "pain_point");
    let rows: Vec<csv::StringRecord> = rd.records().map(|r| r.unwrap()).collect();
    assert_eq!(rows.len(), 2);
    let col = |name: &str| head.iter().position(|h| h == name).unwrap();
    let b = &rows[0]; // newest first
    assert_eq!(&b[col("email")], "b@x.com");
    assert_eq!(&b[col("notes")], "line1\nline2");
    let a = &rows[1];
    assert_eq!(&a[col("first_name")], "'=cmd|' /C calc'!A0", "a formula is made plain text");
    assert_eq!(&a[col("phone")], "'+8801700000000");
    assert_eq!(&a[col("company_name")], "Acme, Inc \"Q\"");
    assert_eq!(&a[col("tags")], "x;y");
    assert_eq!(&a[col("pain_point")], "'@SUM(A1)");
    assert_eq!(&a[col("verification")], "mx_ok");

    // the same filters as the list
    let (_, _, body) = call_raw(&i.h.state, "/v1/leads/export.csv?tag=x", Some(i.key())).await;
    assert_eq!(csv::Reader::from_reader(&body[..]).records().count(), 1);
    let (_, _, body) = call_raw(&i.h.state, "/v1/leads/export.csv?q=nobody", Some(i.key())).await;
    assert_eq!(csv::Reader::from_reader(&body[..]).records().count(), 0);
    let (status, _, _) = call_raw(&i.h.state, "/v1/leads/export.csv", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call_raw(&i.h.state, "/v1/leads/export.csv", Some(&i.keys[&Scope::Sending])).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn export_streams_every_lead_in_pages_and_imports_back(pool: PgPool) {
    let i = I::new(pool).await;
    i.start(&big_csv(2500, 30), &[("tags", "wave1")]).await;
    i.tick().await;
    let (_, _, body) = call_raw(&i.h.state, "/v1/leads/export.csv", Some(i.key())).await;
    let mut rd = csv::Reader::from_reader(&body[..]);
    let emails: Vec<String> = rd.records().map(|r| r.unwrap()[0].to_string()).collect();
    assert_eq!(emails.len(), 2500);
    let unique: std::collections::HashSet<_> = emails.iter().collect();
    assert_eq!(unique.len(), 2500, "no lead twice, none missing");
    // the export imports back without changes
    let text = String::from_utf8(body).unwrap();
    let id = i.start(&text, &[("dedupe_mode", "skip")]).await;
    i.tick().await;
    let r = i.import(id).await;
    assert_eq!((r["status"].as_str(), counters(&r)), (Some("done"), (0, 0, 2500, 0)), "{r}");
}
