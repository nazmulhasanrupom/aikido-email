mod common;
use axum::http::StatusCode;
use chrono::Duration;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::time::Instant;

const HOME: &[(&str, &str)] = &[("origin", ORIGIN)];

async fn login_as(state: &aikido_email::state::AppState, email: &str, password: &str) -> Full {
    call_with(state, "POST", "/v1/auth/login", HOME, Some(json!({ "email": email, "password": password }))).await
}

#[sqlx::test]
async fn first_boot_state_and_setup(pool: PgPool) {
    let state = app_state(pool);
    let s = call(&state, "GET", "/v1/auth/state", None, None).await;
    assert_eq!(s.body["setup_needed"], true);
    assert_eq!(s.body["logged_in"], false);

    let owner = setup_owner(&state).await;
    let s = call_with(&state, "GET", "/v1/auth/state", &[("cookie", &owner.cookie)], None).await;
    assert_eq!(s.body["setup_needed"], false);
    assert_eq!(s.body["logged_in"], true);
    assert_eq!(s.body["user"]["role"], "owner");
    assert_eq!(s.body["workspace_name"], "Acme");
    // the CSRF token can be read again after a page reload
    assert_eq!(s.body["csrf_token"], owner.csrf);
}

#[sqlx::test]
async fn setup_twice_is_409(pool: PgPool) {
    let state = app_state(pool);
    // an old setup code file is removed when the server starts
    std::fs::create_dir_all(&state.config.data_dir).unwrap();
    std::fs::write(state.config.data_dir.join("setup_code.txt"), "ABCD-EFGH-JKMN\n").unwrap();
    aikido_email::users::setup::remove_file(&state.config);
    assert!(!state.config.data_dir.join("setup_code.txt").exists());

    let body = json!({ "name": "Olivia Owner", "email": "owner@example.com", "password": STRONG, "workspace_name": "Acme" });
    let ok = call_with(&state, "POST", "/v1/auth/setup", HOME, Some(body.clone())).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{:?}", ok.body);
    let again = call_with(&state, "POST", "/v1/auth/setup", HOME, Some(body)).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.body["error"]["code"], "setup_done");
}

#[sqlx::test]
async fn setup_checks_the_password_rules(pool: PgPool) {
    let state = app_state(pool);
    for (pw, expect) in [("short", "password_too_short"), ("password1234", "password_too_weak"), ("owner@example.com!!", "password_has_personal_info")] {
        let body = json!({ "name": "Olivia", "email": "owner@example.com", "password": pw, "workspace_name": "Acme" });
        let r = call_with(&state, "POST", "/v1/auth/setup", HOME, Some(body)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{pw}");
        assert_eq!(r.body["error"]["code"], expect, "{pw}");
    }
}

#[sqlx::test]
async fn login_cookie_flags_and_one_error_for_every_failure(pool: PgPool) {
    let state = app_state(pool);
    setup_owner(&state).await;

    let ok = login_as(&state, "OWNER@example.com", STRONG).await;
    assert_eq!(ok.status, StatusCode::OK);
    let cookie = ok.headers.get("set-cookie").unwrap().to_str().unwrap().to_string();
    assert!(cookie.starts_with("__Host-aikido_session="), "{cookie}");
    for flag in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(cookie.contains(flag), "{flag} missing in {cookie}");
    }
    assert!(!cookie.contains("Domain"));
    assert!(ok.body["user"].get("password_hash").is_none());

    let wrong = login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await;
    let unknown = login_as(&state, "nobody@example.com", "wrong wrong wrong wrong").await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body, unknown.body);
    assert_eq!(wrong.body["error"]["message"], "Email or password is wrong.");
    assert!(wrong.headers.get("set-cookie").is_none());
}

#[sqlx::test]
async fn a_new_token_at_every_login(pool: PgPool) {
    let state = app_state(pool);
    setup_owner(&state).await;
    let a = web_from(&login_as(&state, "owner@example.com", STRONG).await);
    let b = web_from(&login_as(&state, "owner@example.com", STRONG).await);
    assert_ne!(a.cookie, b.cookie);
    assert_ne!(a.csrf, b.csrf);
}

#[sqlx::test]
async fn wrong_password_and_unknown_email_take_about_the_same_time(pool: PgPool) {
    let state = app_state(pool);
    setup_owner(&state).await;
    // warm up: the dummy hash is made on first use
    login_as(&state, "warm@example.com", "wrong wrong wrong wrong").await;
    let mut wrong = Vec::new();
    let mut unknown = Vec::new();
    for i in 0..3 {
        let t = Instant::now();
        let r = login_as(&state, "owner@example.com", &format!("wrong password number {i}")).await;
        wrong.push(t.elapsed().as_secs_f64());
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        let t = Instant::now();
        let r = login_as(&state, &format!("nobody{i}@example.com"), &format!("wrong password number {i}")).await;
        unknown.push(t.elapsed().as_secs_f64());
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let best = |v: &[f64]| v.iter().cloned().fold(f64::MAX, f64::min);
    let (w, u) = (best(&wrong), best(&unknown));
    // both did one Argon2 check. Without the dummy hash, the unknown email would be many times faster.
    assert!(u > w * 0.5 && w > u * 0.5, "wrong {w:.4}s, unknown {u:.4}s");
}

#[sqlx::test]
async fn the_sixth_failure_is_delayed_and_the_tenth_locks(pool: PgPool) {
    let mut state = app_state(pool);
    let clock = shared_clock(&mut state, utc(2026, 10, 19, 10, 0));
    setup_owner(&state).await;
    let step = |secs: i64| {
        let mut c = clock.lock().unwrap();
        *c += Duration::seconds(secs);
    };
    // 4 free tries, spaced out so the IP limit never matters
    for _ in 0..4 {
        assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED);
        step(5);
    }
    // the 5th is checked, and then the email must wait 1 second
    assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED);
    let sixth = login_as(&state, "owner@example.com", STRONG).await;
    assert_eq!(sixth.status, StatusCode::TOO_MANY_REQUESTS, "even the right password waits");
    assert_eq!(sixth.body["error"]["code"], "too_many_attempts");
    assert!(sixth.headers.get("retry-after").is_some());
    // after the wait, tries work again. Keep failing: waits grow 2, 4, 8, 16 seconds.
    step(2);
    assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED); // 6th
    step(3);
    assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED); // 7th
    step(5);
    assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED); // 8th
    step(9);
    assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED); // 9th
    step(17);
    assert_eq!(login_as(&state, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED); // 10th: locked now
    step(60);
    let locked = login_as(&state, "owner@example.com", STRONG).await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.body["error"]["message"].as_str().unwrap().contains("minutes"), "{:?}", locked.body);
    // the Team page sees the lock
    let owner_view = {
        step(0);
        let list = aikido_email::users::repo::list(&state.pool).await.unwrap();
        list[0].out(state.now())
    };
    assert!(owner_view.locked_until.is_some());
    // 15 minutes later, the right password works
    step(15 * 60);
    assert_eq!(login_as(&state, "owner@example.com", STRONG).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn the_lockout_works_across_two_server_copies(pool: PgPool) {
    let mut a = app_state(pool.clone());
    let clock = shared_clock(&mut a, utc(2026, 10, 19, 10, 0));
    let mut b = app_state(pool);
    b.clock = a.clock.clone();
    setup_owner(&a).await;
    for i in 0..10 {
        // copy A and copy B take turns
        let copy = if i % 2 == 0 { &a } else { &b };
        assert_eq!(login_as(copy, "owner@example.com", "wrong wrong wrong wrong").await.status, StatusCode::UNAUTHORIZED, "try {i}");
        *clock.lock().unwrap() += Duration::seconds(70);
    }
    // 10 failures counted across both copies: both refuse, even with the right password
    for copy in [&a, &b] {
        assert_eq!(login_as(copy, "owner@example.com", STRONG).await.status, StatusCode::TOO_MANY_REQUESTS);
    }
}

#[sqlx::test]
async fn the_ip_limit_stops_a_spray_of_emails(pool: PgPool) {
    let mut state = app_state(pool);
    let clock = shared_clock(&mut state, utc(2026, 10, 19, 10, 0));
    setup_owner(&state).await;
    for i in 0..20 {
        let r = login_as(&state, &format!("user{i}@example.com"), "wrong wrong wrong wrong").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "try {i}");
    }
    // the 21st try is refused, with a new email and even with the right password
    assert_eq!(login_as(&state, "owner@example.com", STRONG).await.status, StatusCode::TOO_MANY_REQUESTS);
    *clock.lock().unwrap() += Duration::minutes(16);
    assert_eq!(login_as(&state, "owner@example.com", STRONG).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn a_session_without_the_csrf_header_gets_403(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    // a read needs no token
    let get = call_with(&state, "GET", "/v1/keys", &[("cookie", &owner.cookie)], None).await;
    assert_eq!(get.status, StatusCode::OK);
    // a change without the token
    let body = Some(json!({ "scope": "sending" }));
    let no_token = call_with(&state, "POST", "/v1/keys", &[("cookie", &owner.cookie), ("origin", ORIGIN)], body.clone()).await;
    assert_eq!(no_token.status, StatusCode::FORBIDDEN);
    assert_eq!(no_token.body["error"]["code"], "csrf_failed");
    let wrong_token = call_with(&state, "POST", "/v1/keys", &[("cookie", &owner.cookie), ("origin", ORIGIN), ("x-csrf-token", "nope")], body.clone()).await;
    assert_eq!(wrong_token.status, StatusCode::FORBIDDEN);
    // both: works
    assert_eq!(owner.call(&state, "POST", "/v1/keys", body).await.status, StatusCode::CREATED);
}

#[sqlx::test]
async fn a_request_from_another_origin_gets_403(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let body = Some(json!({ "scope": "sending" }));
    for origin in ["http://evil.example", "http://localhost:8081", "https://localhost:8080", "null"] {
        let h = [("cookie", owner.cookie.as_str()), ("x-csrf-token", owner.csrf.as_str()), ("origin", origin)];
        let r = call_with(&state, "POST", "/v1/keys", &h, body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{origin}");
        assert_eq!(r.body["error"]["code"], "csrf_failed");
    }
    // no Origin and no Referer: refused. A Referer from our page is fine.
    let h = [("cookie", owner.cookie.as_str()), ("x-csrf-token", owner.csrf.as_str())];
    assert_eq!(call_with(&state, "POST", "/v1/keys", &h, body.clone()).await.status, StatusCode::FORBIDDEN);
    let h = [("cookie", owner.cookie.as_str()), ("x-csrf-token", owner.csrf.as_str()), ("referer", "http://localhost:8080/w/default/settings/api")];
    assert_eq!(call_with(&state, "POST", "/v1/keys", &h, body).await.status, StatusCode::CREATED);
    // a login from another origin is refused too
    let r = call_with(&state, "POST", "/v1/auth/login", &[("origin", "http://evil.example")], Some(json!({"email": "owner@example.com", "password": STRONG}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn api_keys_work_without_cookies_and_never_allow_credentials(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let admin = &keys[&aikido_email::auth::api_keys::Scope::Admin];
    let r = call_with(&state, "GET", "/v1/keys", &[("authorization", &format!("Bearer {admin}")), ("origin", "http://anything.example")], None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.headers.get("access-control-allow-credentials").is_none());
    let pre = call_with(&state, "OPTIONS", "/v1/keys", &[("origin", "http://anything.example"), ("access-control-request-method", "POST"), ("access-control-request-headers", "x-csrf-token")], None).await;
    assert!(pre.headers.get("access-control-allow-credentials").is_none());
}

#[sqlx::test]
async fn a_member_cannot_use_admin_routes(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;

    assert_eq!(member.call(&state, "GET", "/v1/keys", None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "POST", "/v1/keys", Some(json!({"scope": "sending"}))).await.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "GET", "/v1/users", None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "POST", "/v1/users/invites", Some(json!({"email": "x@example.com", "role": "member"}))).await.status, StatusCode::FORBIDDEN);
    // system switches and defaults
    assert_eq!(member.call(&state, "POST", "/v1/sending/engine/pause", None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "POST", "/v1/generator/pause", None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "PATCH", "/v1/sending/defaults", Some(json!({}))).await.status, StatusCode::FORBIDDEN);
    // but the work routes are open, and the same switches can be read
    assert_eq!(member.call(&state, "GET", "/v1/sending/engine/status", None).await.status, StatusCode::OK);
    assert_eq!(member.call(&state, "GET", "/v1/sending/defaults", None).await.status, StatusCode::OK);
    assert_eq!(member.call(&state, "GET", "/v1/campaigns", None).await.status, StatusCode::OK);
    assert_eq!(member.call(&state, "GET", "/v1/leads", None).await.status, StatusCode::OK);
    // an owner can do all of it
    assert_eq!(owner.call(&state, "GET", "/v1/keys", None).await.status, StatusCode::OK);
    assert_eq!(owner.call(&state, "POST", "/v1/sending/engine/pause", None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn a_password_change_ends_the_other_sessions(pool: PgPool) {
    let state = app_state(pool);
    let first = setup_owner(&state).await;
    let second = web_from(&login_as(&state, "owner@example.com", STRONG).await);
    assert_eq!(second.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::OK);

    let wrong = first.call(&state, "POST", "/v1/users/me/password", Some(json!({"current_password": "nope nope nope nope", "new_password": "an entirely new long phrase 42"}))).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.body["error"]["code"], "wrong_password");
    let weak = first.call(&state, "POST", "/v1/users/me/password", Some(json!({"current_password": STRONG, "new_password": "password1234"}))).await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);

    let ok = first.call(&state, "POST", "/v1/users/me/password", Some(json!({"current_password": STRONG, "new_password": "an entirely new long phrase 42"}))).await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT);
    // the session that changed it stays. The other one is gone.
    assert_eq!(first.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::OK);
    assert_eq!(second.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(login_as(&state, "owner@example.com", STRONG).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(login_as(&state, "owner@example.com", "an entirely new long phrase 42").await.status, StatusCode::OK);
}

#[sqlx::test]
async fn logout_deletes_the_session(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let out = owner.call(&state, "POST", "/v1/auth/logout", None).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.headers.get("set-cookie").unwrap().to_str().unwrap().contains("Max-Age=0"));
    assert_eq!(owner.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
    // the app state says: not logged in
    let s = call_with(&state, "GET", "/v1/auth/state", &[("cookie", &owner.cookie)], None).await;
    assert_eq!(s.body["logged_in"], false);
}

#[sqlx::test]
async fn sessions_expire_when_idle_and_at_the_absolute_limit(pool: PgPool) {
    let mut state = app_state(pool);
    let clock = shared_clock(&mut state, utc(2026, 10, 19, 10, 0));
    let owner = setup_owner(&state).await;
    // used every 2 days: stays alive past the 3 day idle limit
    for _ in 0..5 {
        *clock.lock().unwrap() += Duration::days(2);
        assert_eq!(owner.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::OK);
    }
    // 10 days in. Idle for 4 days: gone.
    *clock.lock().unwrap() += Duration::days(4);
    assert_eq!(owner.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
    // the absolute limit: 30 days, even if used all the time
    let again = web_from(&login_as(&state, "owner@example.com", STRONG).await);
    for _ in 0..15 {
        *clock.lock().unwrap() += Duration::days(2);
        let _ = again.call(&state, "GET", "/v1/users/me", None).await;
    }
    assert_eq!(again.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn invites_work_once(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let r = owner.call(&state, "POST", "/v1/users/invites", Some(json!({"email": "New@Example.com", "role": "member"}))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let link = r.body["link"].as_str().unwrap();
    assert!(link.starts_with("http://localhost:8080/invite/"));
    let token = link.rsplit('/').next().unwrap();
    // the token is not stored in plain form
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM user_invites").fetch_one(&state.pool).await.unwrap();
    assert_ne!(stored, token.as_bytes());

    let info = call(&state, "GET", &format!("/v1/auth/invites/{token}"), None, None).await;
    assert_eq!(info.body["email"], "new@example.com");
    assert_eq!(info.body["role"], "member");
    let list = owner.call(&state, "GET", "/v1/users/invites", None).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);

    let body = json!({"name": "Nia New", "password": "purple monkey dishwasher lamp"});
    let weak = call_with(&state, "POST", &format!("/v1/auth/invites/{token}/accept"), HOME, Some(json!({"name": "Nia", "password": "short"}))).await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);
    let ok = call_with(&state, "POST", &format!("/v1/auth/invites/{token}/accept"), HOME, Some(body.clone())).await;
    assert_eq!(ok.status, StatusCode::CREATED);
    assert_eq!(ok.body["user"]["role"], "member");
    let reuse = call_with(&state, "POST", &format!("/v1/auth/invites/{token}/accept"), HOME, Some(body)).await;
    assert_eq!(reuse.status, StatusCode::NOT_FOUND);
    assert_eq!(call(&state, "GET", "/v1/auth/invites/not-a-token", None, None).await.status, StatusCode::NOT_FOUND);
    assert!(owner.call(&state, "GET", "/v1/users/invites", None).await.body.as_array().unwrap().is_empty());
    // an invite for an existing user is refused
    let dup = owner.call(&state, "POST", "/v1/users/invites", Some(json!({"email": "new@example.com", "role": "admin"}))).await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn expired_invites_are_refused(pool: PgPool) {
    let mut state = app_state(pool);
    let clock = shared_clock(&mut state, utc(2026, 10, 19, 10, 0));
    let owner = setup_owner(&state).await;
    let r = owner.call(&state, "POST", "/v1/users/invites", Some(json!({"email": "late@example.com", "role": "member"}))).await;
    let token = r.body["link"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    *clock.lock().unwrap() += Duration::days(8);
    assert_eq!(call(&state, "GET", &format!("/v1/auth/invites/{token}"), None, None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn team_rules_protect_the_owner(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let admin = add_user(&state, &owner, "admin@example.com", "admin").await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let users = owner.call(&state, "GET", "/v1/users", None).await.body;
    let id_of = |email: &str| users.as_array().unwrap().iter().find(|u| u["email"] == email).unwrap()["id"].as_str().unwrap().to_string();
    let (owner_id, admin_id, member_id) = (id_of("owner@example.com"), id_of("admin@example.com"), id_of("member@example.com"));

    // an admin can manage a member but not touch the owner
    let up = admin.call(&state, "PATCH", &format!("/v1/users/{member_id}"), Some(json!({"role": "admin"}))).await;
    assert_eq!(up.status, StatusCode::OK);
    assert_eq!(up.body["role"], "admin");
    assert_eq!(admin.call(&state, "PATCH", &format!("/v1/users/{owner_id}"), Some(json!({"role": "member"}))).await.status, StatusCode::FORBIDDEN);
    assert_eq!(admin.call(&state, "DELETE", &format!("/v1/users/{owner_id}"), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(owner.call(&state, "PATCH", &format!("/v1/users/{owner_id}"), Some(json!({"status": "disabled"}))).await.status, StatusCode::FORBIDDEN);
    // nobody makes an owner by PATCH
    assert_eq!(owner.call(&state, "PATCH", &format!("/v1/users/{admin_id}"), Some(json!({"role": "owner"}))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // you cannot change or remove yourself
    assert_eq!(admin.call(&state, "PATCH", &format!("/v1/users/{admin_id}"), Some(json!({"role": "member"}))).await.status, StatusCode::FORBIDDEN);
    assert_eq!(admin.call(&state, "DELETE", &format!("/v1/users/{admin_id}"), None).await.status, StatusCode::FORBIDDEN);

    // disabling ends the sessions and blocks the login
    let off = owner.call(&state, "PATCH", &format!("/v1/users/{admin_id}"), Some(json!({"status": "disabled"}))).await;
    assert_eq!(off.status, StatusCode::OK);
    assert_eq!(admin.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(login_as(&state, "admin@example.com", "purple monkey dishwasher lamp").await.status, StatusCode::UNAUTHORIZED);
    // remove
    assert_eq!(owner.call(&state, "DELETE", &format!("/v1/users/{admin_id}"), None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(member.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn unlock_clears_a_locked_email(pool: PgPool) {
    let mut state = app_state(pool);
    let clock = shared_clock(&mut state, utc(2026, 10, 19, 10, 0));
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let _ = member;
    for _ in 0..10 {
        login_as(&state, "member@example.com", "wrong wrong wrong wrong").await;
        *clock.lock().unwrap() += Duration::seconds(70);
    }
    assert_eq!(login_as(&state, "member@example.com", "purple monkey dishwasher lamp").await.status, StatusCode::TOO_MANY_REQUESTS);
    let users = owner.call(&state, "GET", "/v1/users", None).await.body;
    let m = users.as_array().unwrap().iter().find(|u| u["email"] == "member@example.com").unwrap();
    assert!(m["locked_until"].is_string());
    let r = owner.call(&state, "PATCH", &format!("/v1/users/{}", m["id"].as_str().unwrap()), Some(json!({"unlock": true}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body["locked_until"].is_null());
    assert_eq!(login_as(&state, "member@example.com", "purple monkey dishwasher lamp").await.status, StatusCode::OK);
}

#[sqlx::test]
async fn transfer_ownership_needs_the_password_and_an_admin(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let admin = add_user(&state, &owner, "admin@example.com", "admin").await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let users = owner.call(&state, "GET", "/v1/users", None).await.body;
    let id_of = |email: &str| users.as_array().unwrap().iter().find(|u| u["email"] == email).unwrap()["id"].as_str().unwrap().to_string();
    let (admin_id, member_id) = (id_of("admin@example.com"), id_of("member@example.com"));

    let uri = |id: &str| format!("/v1/users/{id}/transfer-ownership");
    assert_eq!(admin.call(&state, "POST", &uri(&admin_id), Some(json!({"password": "x"}))).await.status, StatusCode::FORBIDDEN);
    assert_eq!(owner.call(&state, "POST", &uri(&admin_id), Some(json!({"password": "wrong wrong wrong"}))).await.status, StatusCode::FORBIDDEN);
    assert_eq!(owner.call(&state, "POST", &uri(&member_id), Some(json!({"password": STRONG}))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(owner.call(&state, "POST", &uri(&admin_id), Some(json!({"password": STRONG}))).await.status, StatusCode::NO_CONTENT);
    assert_eq!(admin.call(&state, "GET", "/v1/users/me", None).await.body["role"], "owner");
    assert_eq!(owner.call(&state, "GET", "/v1/users/me", None).await.body["role"], "admin");
    let owners: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 'owner'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(owners, 1);
    let _ = member;
}

#[sqlx::test]
async fn reset_links_work_once_and_end_all_sessions(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let user = aikido_email::users::repo::find_by_email(&state.pool, "owner@example.com").await.unwrap().unwrap();
    let link = aikido_email::users::routes::new_reset_link(&state.pool, state.config.public_url.as_deref(), user.id, state.now()).await.unwrap();
    let token = link.rsplit('/').next().unwrap();
    assert_eq!(call(&state, "GET", &format!("/v1/auth/reset/{token}"), None, None).await.body["email"], "owner@example.com");

    let weak = call_with(&state, "POST", &format!("/v1/auth/reset/{token}"), HOME, Some(json!({"new_password": "short"}))).await;
    assert_eq!(weak.status, StatusCode::UNPROCESSABLE_ENTITY);
    let ok = call_with(&state, "POST", &format!("/v1/auth/reset/{token}"), HOME, Some(json!({"new_password": "an entirely new long phrase 42"}))).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(owner.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(web_from(&ok).call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::OK);
    let again = call_with(&state, "POST", &format!("/v1/auth/reset/{token}"), HOME, Some(json!({"new_password": "another entirely new phrase 43"}))).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn profile_and_preferences(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let r = owner.call(&state, "PATCH", "/v1/users/me", Some(json!({"name": "Olive", "preferences": {"theme": "dark", "x": 1}}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["name"], "Olive");
    let r = owner.call(&state, "PATCH", "/v1/users/me", Some(json!({"preferences": {"x": null, "notifications": {"import_failed": false}}}))).await;
    assert_eq!(r.body["preferences"], json!({"theme": "dark", "notifications": {"import_failed": false}}));
    // a new email needs the current password
    let no_pw = owner.call(&state, "PATCH", "/v1/users/me", Some(json!({"email": "new@example.com"}))).await;
    assert_eq!(no_pw.status, StatusCode::FORBIDDEN);
    let with_pw = owner.call(&state, "PATCH", "/v1/users/me", Some(json!({"email": "NEW@example.com", "current_password": STRONG}))).await;
    assert_eq!(with_pw.status, StatusCode::OK);
    assert_eq!(with_pw.body["email"], "new@example.com");
    assert_eq!(owner.call(&state, "PATCH", "/v1/users/me", Some(json!({"preferences": [1]}))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn my_sessions_list_and_end(pool: PgPool) {
    let state = app_state(pool);
    let first = setup_owner(&state).await;
    let second = web_from(&call_with(&state, "POST", "/v1/auth/login", &[("origin", ORIGIN), ("user-agent", "Firefox test")], Some(json!({"email": "owner@example.com", "password": STRONG}))).await);
    let list = first.call(&state, "GET", "/v1/users/me/sessions", None).await.body;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
    let other = list.iter().find(|s| s["current"] == false).unwrap();
    assert_eq!(other["user_agent"], "Firefox test");
    let end = first.call(&state, "DELETE", &format!("/v1/users/me/sessions/{}", other["id"].as_str().unwrap()), None).await;
    assert_eq!(end.status, StatusCode::NO_CONTENT);
    assert_eq!(second.call(&state, "GET", "/v1/users/me", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(first.call(&state, "DELETE", &format!("/v1/users/me/sessions/{}", uuid::Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn insecure_remote_http_is_refused_unless_allowed(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.public_url = Some("http://mail.example.com".into());
    state.config = std::sync::Arc::new(cfg.clone());
    let r = call_with(&state, "POST", "/v1/auth/login", &[("origin", "http://mail.example.com")], Some(json!({"email": "a@example.com", "password": "x"}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["error"]["code"], "insecure_connection");
    assert_eq!(call(&state, "GET", "/v1/auth/state", None, None).await.body["insecure_connection"], true);
    // allowed on a trusted network: the cookie loses `Secure` and the `__Host-` name
    cfg.insecure_cookies = true;
    state.config = std::sync::Arc::new(cfg);
    let owner = {
        let body = json!({"name": "Olivia Owner", "email": "owner@example.com", "password": STRONG, "workspace_name": "Acme"});
        call_with(&state, "POST", "/v1/auth/setup", &[("origin", "http://mail.example.com")], Some(body)).await
    };
    assert_eq!(owner.status, StatusCode::CREATED, "{:?}", owner.body);
    let cookie = owner.headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.starts_with("aikido_session="), "{cookie}");
    assert!(!cookie.contains("Secure"));
}
