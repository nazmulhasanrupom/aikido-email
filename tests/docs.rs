mod common;
use common::*;
use sqlx::PgPool;

/// Every route must be in the OpenAPI spec, with its key scope. If you add a route and forget
/// the `#[utoipa::path]` or the `routes!` line, this test fails.
const EXPECTED: &[(&str, &str, Option<&str>)] = &[
    ("get", "/health", None),
    ("get", "/v1/accounts/oauth/{provider}/callback", None),
    ("post", "/v1/accounts/oauth/{provider}/start", Some("accounts")),
    ("post", "/v1/accounts/smtp", Some("accounts")),
    ("get", "/v1/accounts", Some("accounts")),
    ("get", "/v1/accounts/{id}", Some("accounts")),
    ("get", "/v1/accounts/{id}/dns-check", Some("accounts")),
    ("get", "/v1/accounts/{id}/health", Some("accounts")),
    ("patch", "/v1/accounts/{id}", Some("accounts")),
    ("post", "/v1/accounts/{id}/test", Some("accounts")),
    ("delete", "/v1/accounts/{id}", Some("accounts")),
    ("get", "/v1/sending/defaults", Some("sending")),
    ("patch", "/v1/sending/defaults", Some("sending")),
    ("get", "/v1/sending/accounts/{id}/settings", Some("sending")),
    ("patch", "/v1/sending/accounts/{id}/settings", Some("sending")),
    ("post", "/v1/sending/accounts/{id}/settings/reset", Some("sending")),
    ("get", "/v1/sending/accounts/{id}/status", Some("sending")),
    ("post", "/v1/sending/accounts/{id}/pause", Some("sending")),
    ("post", "/v1/sending/accounts/{id}/resume", Some("sending")),
    ("get", "/v1/sending/accounts/{id}/preview", Some("sending")),
    ("post", "/v1/sending/jobs", Some("sending")),
    ("get", "/v1/sending/jobs", Some("sending")),
    ("get", "/v1/sending/jobs/{id}", Some("sending")),
    ("delete", "/v1/sending/jobs/{id}", Some("sending")),
    ("post", "/v1/sending/jobs/{id}/requeue", Some("sending")),
    ("get", "/v1/sending/suppressions", Some("sending")),
    ("post", "/v1/sending/suppressions", Some("sending")),
    ("delete", "/v1/sending/suppressions", Some("sending")),
    ("post", "/v1/sending/engine/pause", Some("sending")),
    ("post", "/v1/sending/engine/resume", Some("sending")),
    ("get", "/v1/sending/engine/status", Some("sending")),
    ("get", "/v1/replies/defaults", Some("replies")),
    ("patch", "/v1/replies/defaults", Some("replies")),
    ("get", "/v1/replies/accounts/{id}/settings", Some("replies")),
    ("patch", "/v1/replies/accounts/{id}/settings", Some("replies")),
    ("post", "/v1/replies/accounts/{id}/settings/reset", Some("replies")),
    ("post", "/v1/replies/classify", Some("replies")),
    ("get", "/v1/replies/accounts/{id}/status", Some("replies")),
    ("post", "/v1/replies/accounts/{id}/sync", Some("replies")),
    ("post", "/v1/replies/accounts/{id}/pause", Some("replies")),
    ("post", "/v1/replies/accounts/{id}/resume", Some("replies")),
    ("get", "/v1/replies/messages", Some("replies")),
    ("get", "/v1/replies/messages/{id}", Some("replies")),
    ("patch", "/v1/replies/messages/{id}", Some("replies")),
    ("get", "/v1/replies/lookup", Some("replies")),
    ("post", "/v1/replies/engine/pause", Some("replies")),
    ("post", "/v1/replies/engine/resume", Some("replies")),
    ("get", "/v1/replies/engine/status", Some("replies")),
    ("get", "/v1/keys", Some("admin")),
    ("post", "/v1/followups/sequences", Some("followups")),
    ("get", "/v1/followups/sequences", Some("followups")),
    ("get", "/v1/followups/sequences/{id}", Some("followups")),
    ("patch", "/v1/followups/sequences/{id}", Some("followups")),
    ("put", "/v1/followups/sequences/{id}/steps", Some("followups")),
    ("post", "/v1/followups/sequences/{id}/preview", Some("followups")),
    ("post", "/v1/followups/sequences/{id}/enrollments", Some("followups")),
    ("get", "/v1/followups/enrollments", Some("followups")),
    ("get", "/v1/followups/enrollments/{id}", Some("followups")),
    ("post", "/v1/followups/enrollments/{id}/pause", Some("followups")),
    ("post", "/v1/followups/enrollments/{id}/resume", Some("followups")),
    ("post", "/v1/followups/enrollments/{id}/stop", Some("followups")),
    ("get", "/v1/followups/settings", Some("followups")),
    ("patch", "/v1/followups/settings", Some("followups")),
    ("post", "/v1/followups/pause", Some("followups")),
    ("post", "/v1/followups/resume", Some("followups")),
    ("get", "/v1/followups/status", Some("followups")),
    ("get", "/v1/leads/settings", Some("leads")),
    ("patch", "/v1/leads/settings", Some("leads")),
    ("get", "/v1/leads/fields", Some("leads")),
    ("post", "/v1/leads/fields", Some("leads")),
    ("patch", "/v1/leads/fields/{key}", Some("leads")),
    ("delete", "/v1/leads/fields/{key}", Some("leads")),
    ("get", "/v1/leads/lists", Some("leads")),
    ("post", "/v1/leads/lists", Some("leads")),
    ("patch", "/v1/leads/lists/{id}", Some("leads")),
    ("delete", "/v1/leads/lists/{id}", Some("leads")),
    ("post", "/v1/leads/lists/{id}/import", Some("leads")),
    ("get", "/v1/leads/lists/{id}/rows", Some("leads")),
    ("post", "/v1/leads/lists/{id}/rows", Some("leads")),
    ("delete", "/v1/leads/lists/{id}/rows", Some("leads")),
    ("patch", "/v1/leads/lists/{id}/rows/{row_id}", Some("leads")),
    ("post", "/v1/leads/lists/{id}/columns", Some("leads")),
    ("patch", "/v1/leads/lists/{id}/columns", Some("leads")),
    ("delete", "/v1/leads/lists/{id}/columns", Some("leads")),
    ("post", "/v1/leads/lists/{id}/send-to-campaign", Some("leads")),
    ("post", "/v1/leads", Some("leads")),
    ("get", "/v1/leads", Some("leads")),
    ("post", "/v1/leads/bulk", Some("leads")),
    ("post", "/v1/leads/actions", Some("leads")),
    ("get", "/v1/leads/{id}", Some("leads")),
    ("patch", "/v1/leads/{id}", Some("leads")),
    ("delete", "/v1/leads/{id}", Some("leads")),
    ("get", "/v1/leads/{id}/events", Some("leads")),
    ("get", "/v1/leads/{id}/context", Some("leads")),
    ("post", "/v1/leads/{id}/notes", Some("leads")),
    ("post", "/v1/leads/imports/preview", Some("leads")),
    ("post", "/v1/leads/imports", Some("leads")),
    ("get", "/v1/leads/imports", Some("leads")),
    ("get", "/v1/leads/imports/{id}", Some("leads")),
    ("post", "/v1/leads/imports/{id}/cancel", Some("leads")),
    ("post", "/v1/leads/imports/{id}/mapping/propose", Some("leads")),
    ("get", "/v1/leads/imports/{id}/mapping", Some("leads")),
    ("post", "/v1/leads/imports/{id}/mapping/confirm", Some("leads")),
    ("get", "/v1/leads/mapping-templates", Some("leads")),
    ("delete", "/v1/leads/mapping-templates/{id}", Some("leads")),
    ("get", "/v1/leads/export.csv", Some("leads")),
    ("post", "/v1/campaigns", Some("campaigns")),
    ("get", "/v1/campaigns", Some("campaigns")),
    ("get", "/v1/campaigns/{id}", Some("campaigns")),
    ("patch", "/v1/campaigns/{id}", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/leads", Some("campaigns")),
    ("delete", "/v1/campaigns/{id}/leads", Some("campaigns")),
    ("get", "/v1/campaigns/{id}/leads", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/launch", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/pause", Some("campaigns")),
    ("get", "/v1/campaigns/{id}/email-runs", Some("campaigns")),
    ("get", "/v1/campaigns/{id}/email-runs/{run}", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/email-runs/{run}/cancel", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/emails/estimate", Some("campaigns")),
    ("get", "/v1/campaigns/{id}/checklist", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/duplicate", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/emails/send-test", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/emails/actions", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/emails/generate", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/emails/render", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/emails/test", Some("campaigns")),
    ("get", "/v1/campaigns/{id}/leads/{lead_id}/emails/{step}", Some("campaigns")),
    ("patch", "/v1/campaigns/{id}/leads/{lead_id}/emails/{step}", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/resume", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/archive", Some("campaigns")),
    ("get", "/v1/campaigns/{id}/stats", Some("campaigns")),
    ("post", "/v1/campaigns/feeder/pause", Some("campaigns")),
    ("post", "/v1/campaigns/feeder/resume", Some("campaigns")),
    ("get", "/v1/campaigns/status", Some("campaigns")),
    ("post", "/v1/campaigns/{id}/leads/requeue-verification", Some("campaigns")),
    ("get", "/v1/generator/settings", Some("generator")),
    ("patch", "/v1/generator/settings", Some("generator")),
    ("get", "/v1/generator/providers", Some("generator")),
    ("post", "/v1/generator/providers", Some("generator")),
    ("patch", "/v1/generator/providers/{id}", Some("generator")),
    ("delete", "/v1/generator/providers/{id}", Some("generator")),
    ("post", "/v1/generator/providers/{id}/test", Some("generator")),
    ("post", "/v1/generator/sequences/draft", Some("generator")),
    ("post", "/v1/generator/preview", Some("generator")),
    ("get", "/v1/generator/generations", Some("generator")),
    ("post", "/v1/generator/generations/bulk", Some("generator")),
    ("get", "/v1/generator/generations/{id}", Some("generator")),
    ("patch", "/v1/generator/generations/{id}", Some("generator")),
    ("post", "/v1/generator/generations/{id}/approve", Some("generator")),
    ("post", "/v1/generator/generations/{id}/reject", Some("generator")),
    ("get", "/v1/generator/usage", Some("generator")),
    ("get", "/v1/generator/status", Some("generator")),
    ("post", "/v1/generator/pause", Some("generator")),
    ("post", "/v1/generator/resume", Some("generator")),
    ("get", "/v1/generator/campaigns/{id}", Some("generator")),
    ("patch", "/v1/generator/campaigns/{id}", Some("generator")),
    ("get", "/v1/integrations/apps", Some("integrations")),
    ("post", "/v1/integrations/apps/{app}/install", Some("integrations")),
    ("patch", "/v1/integrations/apps/{app}", Some("integrations")),
    ("delete", "/v1/integrations/apps/{app}", Some("integrations")),
    ("post", "/v1/personalization/sources", Some("personalization")),
    ("get", "/v1/personalization/sources", Some("personalization")),
    ("post", "/v1/personalization/sources/cv", Some("personalization")),
    ("delete", "/v1/personalization/sources/{id}", Some("personalization")),
    ("delete", "/v1/personalization", Some("personalization")),
    ("post", "/v1/personalization/profile/generate", Some("personalization")),
    ("post", "/v1/personalization/business/generate", Some("personalization")),
    ("put", "/v1/personalization/business", Some("personalization")),
    ("put", "/v1/personalization/profile", Some("personalization")),
    ("get", "/v1/personalization/profile", Some("personalization")),
    ("get", "/v1/personalization/profile/versions", Some("personalization")),
    ("get", "/v1/fields", Some("leads")),
    ("get", "/v1/prompts/defaults", Some("campaigns")),
    ("get", "/v1/verify/reoon/balance", Some("integrations")),
    ("get", "/v1/verify/reoon/status", Some("integrations")),
    ("get", "/v1/verify/reoon/leads", Some("integrations")),
    ("get", "/v1/verify/reoon/leads/{id}", Some("integrations")),
    ("post", "/v1/verify/reoon/runs", Some("integrations")),
    ("get", "/v1/verify/reoon/runs", Some("integrations")),
    ("get", "/v1/verify/reoon/runs/{id}", Some("integrations")),
    ("post", "/v1/verify/reoon/runs/{id}/cancel", Some("integrations")),
    ("post", "/v1/verify/reoon/runs/{id}/resume", Some("integrations")),
    ("get", "/v1/verify/reoon/settings", Some("integrations")),
    ("patch", "/v1/verify/reoon/settings", Some("integrations")),
    ("post", "/v1/verify/reoon/pause", Some("integrations")),
    ("post", "/v1/verify/reoon/resume", Some("integrations")),
    ("post", "/v1/keys", Some("admin")),
    ("post", "/v1/keys/{id}/rotate", Some("admin")),
    ("delete", "/v1/keys/{id}", Some("admin")),
    // the web app: login (no key), team (a session), helpers (a key or a session), numbers for charts
    ("get", "/v1/auth/state", None),
    ("post", "/v1/auth/setup", None),
    ("post", "/v1/auth/login", None),
    ("post", "/v1/auth/logout", None),
    ("get", "/v1/auth/invites/{token}", None),
    ("post", "/v1/auth/invites/{token}/accept", None),
    ("get", "/v1/auth/reset/{token}", None),
    ("post", "/v1/auth/reset/{token}", None),
    ("get", "/v1/users/me", None),
    ("patch", "/v1/users/me", None),
    ("post", "/v1/users/me/password", None),
    ("get", "/v1/users/me/sessions", None),
    ("delete", "/v1/users/me/sessions/{id}", None),
    ("get", "/v1/users", None),
    ("patch", "/v1/users/{id}", None),
    ("delete", "/v1/users/{id}", None),
    ("post", "/v1/users/invites", None),
    ("get", "/v1/users/invites", None),
    ("delete", "/v1/users/invites/{id}", None),
    ("post", "/v1/users/{id}/transfer-ownership", None),
    ("post", "/v1/users/{id}/reset-link", None),
    ("get", "/v1/ui/config", None),
    ("get", "/v1/ui/onboarding", None),
    ("patch", "/v1/ui/onboarding", Some("admin")),
    ("get", "/v1/ui/alerts", None),
    ("get", "/v1/ui/workspace", None),
    ("patch", "/v1/ui/workspace", Some("admin")),
    ("get", "/v1/reports/daily", Some("campaigns")),
    ("get", "/v1/reports/accounts", Some("campaigns")),
    // open tracking: the API (scope `tracking`) and the public image routes
    ("get", "/v1/tracking/domains", Some("tracking")),
    ("put", "/v1/tracking/domains/{id}", Some("tracking")),
    ("post", "/v1/tracking/domains/{id}/check", Some("tracking")),
    ("get", "/v1/campaigns/{id}/tracking", Some("tracking")),
    ("put", "/v1/campaigns/{id}/tracking", Some("tracking")),
    ("post", "/v1/campaigns/{id}/tracking/preview", Some("tracking")),
    ("post", "/v1/campaigns/{id}/tracking/refresh-image", Some("tracking")),
    ("get", "/v1/campaigns/{id}/opens", Some("tracking")),
    ("get", "/v1/leads/{id}/opens", Some("tracking")),
    ("get", "/i/{file}", None),
    ("get", "/p/{token}", None),
    ("get", "/.well-known/acme-challenge/{token}", None),
];

/// Routes that need no key and no session. Every other route must refuse a call with neither.
fn is_public(path: &str) -> bool {
    path == "/health"
        || path == "/v1/accounts/oauth/{provider}/callback"
        || path.starts_with("/v1/auth/")
        || path.starts_with("/v1/unsubscribe/")
        || path.starts_with("/i/")
        || path.starts_with("/p/")
        || path.starts_with("/.well-known/")
}

#[sqlx::test]
async fn every_route_is_in_the_spec_with_its_scope(pool: PgPool) {
    let state = app_state(pool);
    let spec = call(&state, "GET", "/openapi.json", None, None).await.body;
    let paths = spec["paths"].as_object().unwrap();

    let mut in_spec = Vec::new();
    for (path, item) in paths {
        for (method, op) in item.as_object().unwrap() {
            let scope = op["security"][0]["api_key"][0].as_str();
            in_spec.push((method.clone(), path.clone(), scope.map(str::to_string)));
        }
    }
    for (method, path, scope) in EXPECTED {
        let found = in_spec.iter().find(|(m, p, _)| m == method && p == path);
        let found = found.unwrap_or_else(|| panic!("{method} {path} is not in the OpenAPI spec"));
        assert_eq!(found.2.as_deref(), *scope, "wrong scope for {method} {path}");
    }
    assert_eq!(in_spec.len(), EXPECTED.len(), "the spec has routes this test does not know: {in_spec:?}");

    // /docs and /openapi.json need no key.
    assert_eq!(call(&state, "GET", "/docs", None, None).await.status, axum::http::StatusCode::OK);
}

#[sqlx::test]
async fn every_route_but_the_public_ones_refuses_a_call_with_no_key_and_no_session(pool: PgPool) {
    let state = app_state(pool);
    let spec = call(&state, "GET", "/openapi.json", None, None).await.body;
    let mut checked = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        if is_public(path) {
            continue;
        }
        let uri = path.replace("{key}", "x").replace("{provider}", "gmail").replace("{token}", "x").replace("{file}", "x").replace("{id}", &uuid::Uuid::now_v7().to_string());
        for method in item.as_object().unwrap().keys() {
            let r = call(&state, &method.to_uppercase(), &uri, None, None).await;
            assert_eq!(r.status, axum::http::StatusCode::UNAUTHORIZED, "{method} {path} answers without any login: {}", r.body);
            checked += 1;
        }
    }
    assert!(checked > 100, "only {checked} routes were checked");
}
