//! `/v1/accounts/*`
use super::model::*;
use super::{oauth, repo};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::email_management::imapsmtp::connect as imap;
use crate::email_management::{gmailconnection::connect as gmail, msconnection::connect as microsoft};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::sending::senders::smtp::{SmtpAuth, build_transport, is_local_host, test_login};
use crate::sending::tokens;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Routes that need a key with scope `accounts`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(oauth_start))
        .routes(routes!(create_smtp))
        .routes(routes!(list_accounts))
        .routes(routes!(get_account, patch_account, delete_account))
        .routes(routes!(test_account))
        .routes(routes!(dns_check))
        .routes(routes!(health))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Accounts), require_scope))
}

/// Routes with no key. The OAuth return is protected by the sealed `state` value.
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(oauth_callback))
}

fn parse_provider(p: &str) -> ApiResult<Provider> {
    match Provider::parse(p) {
        Some(x) if x.is_oauth() => Ok(x),
        _ => Err(ApiError::not_found("unknown_provider", format!("'{p}' is not an OAuth provider. Use 'gmail' or 'microsoft'."))),
    }
}

fn redirect_url(state: &AppState, provider: Provider) -> ApiResult<String> {
    let base = state.config.public_url.as_deref().ok_or_else(|| {
        ApiError::bad_request("public_url_not_set", "Set AIKIDO_PUBLIC_URL to use the OAuth login.")
    })?;
    Ok(format!("{base}/v1/accounts/oauth/{}/callback", provider.as_str()))
}

// ---------- OAuth ----------

#[derive(Serialize, ToSchema)]
pub struct OAuthStartReply {
    /// Send the user to this URL to sign in.
    pub auth_url: String,
}

/// Returns the login URL for `gmail` or `microsoft`.
#[utoipa::path(post, path = "/v1/accounts/oauth/{provider}/start", tag = "accounts",
    params(("provider" = String, Path, description = "gmail or microsoft")),
    responses((status = 200, body = OAuthStartReply)),
    security(("api_key" = ["accounts"])),
    description = "Scope: `accounts`.")]
async fn oauth_start(State(st): State<AppState>, ApiPath(provider): ApiPath<String>) -> ApiResult<ApiJson<OAuthStartReply>> {
    let provider = parse_provider(&provider)?;
    let redirect = redirect_url(&st, provider)?;
    let crypto = st.crypto.clone();
    let make_state = move |verifier: &str| oauth::seal(&crypto, provider, verifier);
    let started = match provider {
        Provider::Gmail => gmail::web_start(&redirect, make_state),
        _ => microsoft::web_start(&redirect, make_state),
    };
    let auth_url = started.map_err(|e| ApiError::bad_request("oauth_not_configured", e.to_string()))?;
    Ok(ApiJson(OAuthStartReply { auth_url }))
}

#[derive(Deserialize, IntoParams)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The data for the page script, safe inside `<script>`: no `<`, `>` or `&` in it.
fn script_json(value: &serde_json::Value) -> String {
    value.to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

/// The page the OAuth popup shows. When a web app opened it, the script sends the result
/// to that window (same origin only) and closes the popup. In a plain tab it stays as text.
fn popup_page(ok: bool, email: Option<&str>, code: Option<&str>, text: &str) -> String {
    let message = serde_json::json!({ "type": "aikido:oauth", "ok": ok, "email": email, "code": code });
    format!(
        "<!doctype html><meta charset=utf-8><title>{}</title><p>{}</p>\
         <script>(function(){{var m={};try{{if(window.opener){{window.opener.postMessage(m,location.origin);window.close();}}}}catch(e){{}}}})();</script>",
        if ok { "Connected" } else { "Not connected" },
        escape_html(text),
        script_json(&message)
    )
}

fn wants_html(headers: &axum::http::HeaderMap) -> bool {
    headers.get(axum::http::header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"))
}

/// Where the provider sends the user back. No key: the signed `state` value protects it.
#[utoipa::path(get, path = "/v1/accounts/oauth/{provider}/callback", tag = "accounts",
    params(("provider" = String, Path, description = "gmail or microsoft"), CallbackQuery),
    responses((status = 200, description = "A small page that says the account is connected. It tells the window that opened it (`postMessage` `{ type: \"aikido:oauth\", ok, email, code }`) and closes itself.", content_type = "text/html")),
    description = "No key needed. Protected by the signed `state` value. A browser (it asks for HTML) gets an HTML page for errors too, with the same status code. Other callers get the JSON error.")]
async fn oauth_callback(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    ApiPath(provider): ApiPath<String>,
    ApiQuery(q): ApiQuery<CallbackQuery>,
) -> Response {
    match finish_oauth(&st, &provider, q).await {
        Ok(email) => {
            tracing::info!(%email, "account connected");
            Html(popup_page(true, Some(&email), None, &format!("Connected: {email}. You can close this tab."))).into_response()
        }
        Err(e) if wants_html(&headers) => (e.status, Html(popup_page(false, None, Some(&e.code), &e.message))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn finish_oauth(st: &AppState, provider: &str, q: CallbackQuery) -> ApiResult<String> {
    let provider = parse_provider(provider)?;
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or_default();
        return Err(ApiError::bad_request("oauth_denied", format!("The login failed: {err} {desc}")));
    }
    let (Some(code), Some(state_value)) = (q.code, q.state) else {
        return Err(ApiError::bad_request("oauth_bad_callback", "The callback needs 'code' and 'state'."));
    };
    let payload = oauth::open(&st.crypto, &state_value, provider).map_err(|m| ApiError::bad_request("oauth_bad_state", m))?;
    let redirect = redirect_url(st, provider)?;
    let connected = match provider {
        Provider::Gmail => gmail::web_finish(&redirect, code, payload.verifier).await,
        _ => microsoft::web_finish(&redirect, code, payload.verifier).await,
    }
    .map_err(|e| ApiError::bad_request("oauth_exchange_failed", e.to_string()))?;

    let email = connected.email.clone();
    let account = repo::save_oauth_login(&st.pool, &st.crypto, provider, connected).await?;
    tracing::info!(account_id = %account.id, "account saved from the web login");
    Ok(email)
}

// ---------- SMTP / IMAP ----------

#[derive(Deserialize, ToSchema)]
pub struct ServerInput {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    /// Default: the email address.
    pub username: Option<String>,
    pub password: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateSmtpAccount {
    pub email: String,
    pub display_name: Option<String>,
    pub smtp: ServerInput,
    pub imap: Option<ServerInput>,
}

fn check_server(kind: &str, s_host: &str, tls: TlsMode) -> ApiResult<()> {
    if s_host.trim().is_empty() || s_host.contains(char::is_whitespace) {
        return Err(ApiError::unprocessable("invalid_host", format!("The {kind} host is not valid.")));
    }
    if tls == TlsMode::Plain && !is_local_host(s_host) {
        return Err(ApiError::unprocessable(
            "insecure_tls",
            format!("{kind} without encryption is allowed only for localhost. Use 'implicit' or 'starttls'."),
        ));
    }
    Ok(())
}

fn to_imap_server(h: &HostInfo, username: &str, password: &SecretString) -> Option<imap::Server> {
    use secrecy::ExposeSecret;
    let security = match h.tls {
        TlsMode::Implicit => imap::Security::Tls,
        TlsMode::Starttls => imap::Security::StartTls,
        TlsMode::Plain => return None, // localhost test servers only: not tested
    };
    Some(imap::Server {
        host: h.host.clone(),
        port: h.port,
        security,
        username: username.to_string(),
        password: password.expose_secret().to_string(),
    })
}

/// Logs in to SMTP (and IMAP, if given). Saves nothing. Error text never has a password.
async fn test_servers(
    smtp: Option<(&HostInfo, &str, &SecretString)>,
    imap_cfg: Option<(&HostInfo, &str, &SecretString)>,
) -> ApiResult<()> {
    if let Some((h, user, pass)) = smtp {
        let transport = build_transport(
            &h.host,
            h.port,
            h.tls,
            SmtpAuth::Password { username: user.to_string(), password: pass.clone() },
        )
        .map_err(|e| ApiError::unprocessable("smtp_login_failed", e.describe()))?;
        test_login(&transport)
            .await
            .map_err(|e| ApiError::unprocessable("smtp_login_failed", format!("SMTP login failed: {}", e.describe())))?;
    }
    if let Some((h, user, pass)) = imap_cfg {
        if let Some(server) = to_imap_server(h, user, pass) {
            imap::test_imap(&server)
                .await
                .map_err(|e| ApiError::unprocessable("imap_login_failed", format!("IMAP login failed: {e}")))?;
        }
    }
    Ok(())
}

/// Adds an SMTP/IMAP account. Tests the login before saving.
#[utoipa::path(post, path = "/v1/accounts/smtp", tag = "accounts",
    request_body = CreateSmtpAccount,
    responses((status = 201, body = AccountView)),
    security(("api_key" = ["accounts"])),
    description = "Scope: `accounts`. The SMTP login (and the IMAP login, if given) is tested before the account is saved.")]
async fn create_smtp(State(st): State<AppState>, ApiJson(body): ApiJson<CreateSmtpAccount>) -> ApiResult<(StatusCode, ApiJson<AccountView>)> {
    let email = body.email.trim().to_lowercase();
    if email.parse::<lettre::Address>().is_err() {
        return Err(ApiError::unprocessable("invalid_email", format!("'{email}' is not a valid email address.")));
    }
    check_server("SMTP", &body.smtp.host, body.smtp.tls)?;
    if let Some(i) = &body.imap {
        check_server("IMAP", &i.host, i.tls)?;
    }
    let smtp_user = body.smtp.username.clone().unwrap_or_else(|| email.clone());
    let smtp_pass = SecretString::from(body.smtp.password);
    let smtp_host = HostInfo { host: body.smtp.host.trim().to_string(), port: body.smtp.port, tls: body.smtp.tls };
    let imap_parts = body.imap.map(|i| {
        let user = i.username.clone().unwrap_or_else(|| email.clone());
        (HostInfo { host: i.host.trim().to_string(), port: i.port, tls: i.tls }, user, SecretString::from(i.password))
    });

    test_servers(Some((&smtp_host, &smtp_user, &smtp_pass)), imap_parts.as_ref().map(|(h, u, p)| (h, u.as_str(), p))).await?;

    let secrets = Secrets::Password(PasswordSecrets {
        smtp_username: Some(smtp_user),
        smtp_password: Some(smtp_pass),
        imap_username: imap_parts.as_ref().map(|p| p.1.clone()),
        imap_password: imap_parts.as_ref().map(|p| p.2.clone()),
    });
    let new = repo::NewAccount {
        email,
        provider: Provider::Smtp,
        display_name: body.display_name.unwrap_or_default(),
        smtp: Some(smtp_host),
        imap: imap_parts.map(|p| p.0),
        token_expires_at: None,
    };
    let account = repo::upsert_connected(&st.pool, &st.crypto, new, &secrets).await?;
    Ok((StatusCode::CREATED, ApiJson(AccountView::from(&account))))
}

/// Lists accounts. Never returns secrets.
#[utoipa::path(get, path = "/v1/accounts", tag = "accounts",
    responses((status = 200, body = Vec<AccountView>)),
    security(("api_key" = ["accounts"])), description = "Scope: `accounts`.")]
async fn list_accounts(State(st): State<AppState>) -> ApiResult<ApiJson<Vec<AccountView>>> {
    let accounts = repo::list(&st.pool).await?;
    Ok(ApiJson(accounts.iter().map(AccountView::from).collect()))
}

async fn find(st: &AppState, id: Uuid) -> ApiResult<EmailAccount> {
    repo::get(&st.pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("account_not_found", "No account with this id."))
}

/// One account.
#[utoipa::path(get, path = "/v1/accounts/{id}", tag = "accounts",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = AccountView)),
    security(("api_key" = ["accounts"])), description = "Scope: `accounts`.")]
async fn get_account(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<AccountView>> {
    Ok(ApiJson(AccountView::from(&find(&st, id).await?)))
}

/// Fields to change on a server. Missing fields keep their value.
#[derive(Deserialize, ToSchema)]
pub struct ServerPatch {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls: Option<TlsMode>,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct PatchAccount {
    pub display_name: Option<String>,
    pub smtp: Option<ServerPatch>,
    pub imap: Option<ServerPatch>,
}

/// The server settings an account has now, if it has them.
fn host_of(host: &Option<String>, port: Option<i32>, tls: Option<TlsMode>) -> Option<HostInfo> {
    Some(HostInfo { host: host.clone()?, port: u16::try_from(port?).ok()?, tls: tls? })
}

/// Applies a `ServerPatch` on top of the current settings and login.
/// Returns the new host settings. `user` and `pass` are updated in place.
fn merge_server(
    kind: &str,
    patch: &ServerPatch,
    current: Option<HostInfo>,
    user: &mut Option<String>,
    pass: &mut Option<SecretString>,
) -> ApiResult<HostInfo> {
    let missing = |field: &str| {
        ApiError::unprocessable(
            "incomplete_server",
            format!("This account has no {kind} settings yet. Give a host, a port, and a tls mode ('{field}' is missing)."),
        )
    };
    let host = patch.host.clone().or_else(|| current.as_ref().map(|c| c.host.clone())).ok_or_else(|| missing("host"))?;
    let port = patch.port.or(current.as_ref().map(|c| c.port)).ok_or_else(|| missing("port"))?;
    let tls = patch.tls.or(current.as_ref().map(|c| c.tls)).ok_or_else(|| missing("tls"))?;
    if let Some(u) = &patch.username {
        *user = Some(u.clone());
    }
    if let Some(p) = &patch.password {
        *pass = Some(SecretString::from(p.clone()));
    }
    let host = HostInfo { host: host.trim().to_string(), port, tls };
    check_server(kind, &host.host, host.tls)?;
    Ok(host)
}

/// Changes the display name or the SMTP/IMAP details.
#[utoipa::path(patch, path = "/v1/accounts/{id}", tag = "accounts",
    params(("id" = Uuid, Path)), request_body = PatchAccount,
    responses((status = 200, body = AccountView)),
    security(("api_key" = ["accounts"])),
    description = "Scope: `accounts`. If server details or passwords change, the login is tested again before saving.")]
async fn patch_account(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<PatchAccount>,
) -> ApiResult<ApiJson<AccountView>> {
    let account = find(&st, id).await?;
    let mut changes = repo::AccountChanges { display_name: body.display_name, ..Default::default() };

    if body.smtp.is_some() || body.imap.is_some() {
        if account.provider != Provider::Smtp {
            return Err(ApiError::unprocessable("not_smtp_account", "Only SMTP/IMAP accounts have server details."));
        }
        let Secrets::Password(mut secrets) = repo::load_secrets(&st.crypto, &account)? else {
            return Err(ApiError::internal());
        };
        let new_smtp = match &body.smtp {
            Some(p) => Some(merge_server(
                "SMTP",
                p,
                host_of(&account.smtp_host, account.smtp_port, account.smtp_tls),
                &mut secrets.smtp_username,
                &mut secrets.smtp_password,
            )?),
            None => None,
        };
        let new_imap = match &body.imap {
            Some(p) => Some(merge_server(
                "IMAP",
                p,
                host_of(&account.imap_host, account.imap_port, account.imap_tls),
                &mut secrets.imap_username,
                &mut secrets.imap_password,
            )?),
            None => None,
        };
        // Test only the server that changed.
        let empty = SecretString::from(String::new());
        let smtp_user = secrets.smtp_username.clone().unwrap_or_else(|| account.email.clone());
        let imap_user = secrets.imap_username.clone().unwrap_or_else(|| account.email.clone());
        let smtp_pass = secrets.smtp_password.clone().unwrap_or_else(|| empty.clone());
        let imap_pass = secrets.imap_password.clone().unwrap_or(empty);
        test_servers(
            new_smtp.as_ref().map(|h| (h, smtp_user.as_str(), &smtp_pass)),
            new_imap.as_ref().map(|h| (h, imap_user.as_str(), &imap_pass)),
        )
        .await?;
        // The tests above passed. The IMAP login of a plain localhost server is not tested, but it is saved too.
        changes.reconnect = new_smtp.is_some();
        changes.read_ok = new_imap.is_some();
        changes.smtp = new_smtp;
        changes.imap = new_imap;
        changes.secrets = Some(Secrets::Password(secrets));
    }

    let updated = repo::apply_changes(&st.pool, &st.crypto, id, changes)
        .await?
        .ok_or_else(|| ApiError::not_found("account_not_found", "No account with this id."))?;
    Ok(ApiJson(AccountView::from(&updated)))
}

#[derive(Serialize, ToSchema)]
pub struct TestReply {
    pub ok: bool,
    pub detail: String,
}

/// Checks the login works. Sends nothing.
#[utoipa::path(post, path = "/v1/accounts/{id}/test", tag = "accounts",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = TestReply)),
    security(("api_key" = ["accounts"])),
    description = "Scope: `accounts`. OAuth accounts: gets a fresh token. SMTP/IMAP accounts: logs in. Sends no email.")]
async fn test_account(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<TestReply>> {
    let account = find(&st, id).await?;
    if !account.has_credentials() {
        return Ok(ApiJson(TestReply { ok: false, detail: "No credentials are saved. Connect the account again.".into() }));
    }
    let secrets = repo::load_secrets(&st.crypto, &account)?;
    let result: Result<(), String> = match secrets {
        Secrets::OAuth(o) => tokens::refresh(&st.pool, &st.crypto, &account, &o, st.now())
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Secrets::Password(p) => {
            let smtp = host_of(&account.smtp_host, account.smtp_port, account.smtp_tls);
            let imap_info = host_of(&account.imap_host, account.imap_port, account.imap_tls);
            let empty = SecretString::from(String::new());
            let (su, sp) = (p.smtp_username.clone().unwrap_or_else(|| account.email.clone()), p.smtp_password.clone().unwrap_or_else(|| empty.clone()));
            let (iu, ip) = (p.imap_username.clone().unwrap_or_else(|| account.email.clone()), p.imap_password.clone().unwrap_or(empty));
            let tested = test_servers(smtp.as_ref().map(|h| (h, su.as_str(), &sp)), imap_info.as_ref().map(|h| (h, iu.as_str(), &ip))).await;
            // The IMAP login decides if the inbox can be read.
            if imap_info.is_some() {
                match &tested {
                    Ok(()) => crate::replies::repo::set_read_access(&st.pool, id, None, true, "ok").await?,
                    Err(e) if e.code == "imap_login_failed" => crate::replies::repo::set_read_access(&st.pool, id, None, false, "login_failed").await?,
                    Err(_) => {}
                }
            }
            tested.map_err(|e| e.message)
        }
    };
    Ok(ApiJson(match result {
        Ok(()) => TestReply { ok: true, detail: "The login works.".into() },
        Err(m) => TestReply { ok: false, detail: m },
    }))
}

#[derive(Serialize, ToSchema)]
pub struct DeleteReply {
    pub deleted: bool,
    pub cancelled_jobs: u64,
}

/// Wipes the secrets, soft-deletes the account, and cancels its queued jobs.
#[utoipa::path(delete, path = "/v1/accounts/{id}", tag = "accounts",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = DeleteReply)),
    security(("api_key" = ["accounts"])), description = "Scope: `accounts`.")]
async fn delete_account(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<DeleteReply>> {
    match repo::soft_delete(&st.pool, id).await? {
        Some(n) => Ok(ApiJson(DeleteReply { deleted: true, cancelled_jobs: n })),
        None => Err(ApiError::not_found("account_not_found", "No account with this id.")),
    }
}

/// The health of an account: DNS records, sending power and blacklists, as one score from 0 to 100.
#[utoipa::path(get, path = "/v1/accounts/{id}/health", tag = "accounts",
    params(("id" = Uuid, Path), DnsQuery),
    responses((status = 200, body = super::health::HealthReport), (status = 404, description = "No such account")),
    security(("api_key" = ["accounts"])),
    description = "Scope: `accounts`. DNS health (MX 40, SPF 20, DKIM 20, DMARC 10, A 10; a warning gives half), \
        sending power (send success and bounces of the last 30 days, from 10 sent emails) and DNS blacklists. \
        The score is the mean of DNS health and sending power, minus 20 per listing. DNS and blacklist answers are kept for one hour.")]
async fn health(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<DnsQuery>) -> ApiResult<ApiJson<super::health::HealthReport>> {
    let a = find(&st, id).await?;
    Ok(ApiJson(super::health::report(&st, &a, q.selector.as_deref()).await?))
}

#[cfg(test)]
mod popup_tests {
    use super::*;

    #[test]
    fn the_popup_page_reports_to_its_opener_and_cannot_break_out_of_the_script() {
        let page = popup_page(true, Some("</script><b>x@example.com"), None, "Connected: </script><b>x@example.com");
        assert!(page.contains("window.opener.postMessage(m,location.origin)"));
        assert!(page.contains("window.close()"));
        assert!(page.contains("\"type\":\"aikido:oauth\"") && page.contains("\"ok\":true"));
        // the only `</script>` is the real end tag
        assert_eq!(page.matches("</script>").count(), 1, "{page}");
        assert!(!page.contains("<b>"));
        let failed = popup_page(false, None, Some("oauth_denied"), "The login failed");
        assert!(failed.contains("\"ok\":false") && failed.contains("\"code\":\"oauth_denied\""));
    }
}

#[derive(Deserialize, IntoParams)]
pub struct DnsQuery {
    /// A DKIM selector to look at first (for example `google`).
    selector: Option<String>,
}

/// Check the sending domain of an account: SPF, DKIM, DMARC and MX.
#[utoipa::path(get, path = "/v1/accounts/{id}/dns-check", tag = "accounts",
    params(("id" = Uuid, Path), DnsQuery),
    responses((status = 200, body = super::dns_check::DnsReport), (status = 404, description = "No such account")),
    security(("api_key" = ["accounts"])),
    description = "Scope: `accounts`. Each check is `ok`, `warn`, `fail` or `unknown`, with the DNS record to add when it is not ok. Answers are kept for one hour. \
        DKIM keys sit at a selector the sender chose, so only common selectors and the one in `selector` are looked at.")]
async fn dns_check(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<DnsQuery>) -> ApiResult<ApiJson<super::dns_check::DnsReport>> {
    let a = find(&st, id).await?;
    let domain = a.email.rsplit('@').next().unwrap_or_default().to_string();
    let provider = AccountView::from(&a).provider.as_str().to_string();
    Ok(ApiJson(st.dns.check(st.mx.as_ref(), &domain, &provider, q.selector.as_deref(), st.now()).await))
}
