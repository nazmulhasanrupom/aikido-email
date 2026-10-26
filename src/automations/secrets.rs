//! Secrets (build map 11.3): named values that a workflow uses as `${{ secret('NAME') }}` or in the `auth`
//! block of HTTP Request. The value is sealed with AES-256-GCM (AAD = `automation_secret:` + name). It is never
//! returned and never logged. Before any output, error or log line of a run is stored, every secret value that
//! the run can use (and its base64 and URL-encoded forms) is replaced with `[secret:NAME]`.
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::automations::definition::WorkflowDefinition;
use crate::automations::expr::{ExprEngine, expression_strings};
use crate::automations::routes::caller;
use crate::crypto::Crypto;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

/// The shortest value we keep. A shorter text would be replaced all over the data of a run.
pub const MIN_VALUE_LEN: usize = 4;
pub const MAX_VALUE_LEN: usize = 10_000;

pub fn valid_name(name: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^[A-Z][A-Z0-9_]{0,63}$").unwrap());
    RE.is_match(name)
}

fn aad(name: &str) -> Vec<u8> {
    format!("automation_secret:{name}").into_bytes()
}

/// `key version (4 bytes) | nonce (12 bytes) | ciphertext`.
pub fn seal(crypto: &Crypto, name: &str, value: &str) -> Result<Vec<u8>, String> {
    let sealed = crypto.encrypt(value.as_bytes(), &aad(name)).map_err(|e| e.to_string())?;
    let mut out = sealed.key_version.to_be_bytes().to_vec();
    out.extend_from_slice(&sealed.nonce);
    out.extend_from_slice(&sealed.ciphertext);
    Ok(out)
}

pub fn open(crypto: &Crypto, name: &str, packed: &[u8]) -> Result<String, String> {
    if packed.len() < 16 {
        return Err("the stored secret is damaged".into());
    }
    let version = i32::from_be_bytes([packed[0], packed[1], packed[2], packed[3]]);
    let plain = crypto.decrypt(&packed[16..], &packed[4..16], version, &aad(name)).map_err(|e| e.to_string())?;
    String::from_utf8(plain.to_vec()).map_err(|_| "the stored secret is not text".to_string())
}

/// The secret values a run can use.
#[derive(Default, Clone)]
pub struct Secrets {
    values: BTreeMap<String, String>,
    /// What to look for, with the replacement. Longest first, so a long form is cut out before a short one inside it.
    needles: Vec<(String, String)>,
}

impl Secrets {
    pub fn new(values: BTreeMap<String, String>) -> Self {
        let mut needles: Vec<(String, String)> = vec![];
        for (name, v) in &values {
            if v.len() < MIN_VALUE_LEN {
                continue;
            }
            let tag = format!("[secret:{name}]");
            let mut forms = vec![v.clone(), STANDARD.encode(v), URL_SAFE.encode(v), URL_SAFE_NO_PAD.encode(v), crate::automations::internal_call::urlencode(v)];
            // the value inside a Basic or Bearer header written by hand is covered by the value itself
            forms.sort();
            forms.dedup();
            for f in forms {
                needles.push((f, tag.clone()));
            }
        }
        needles.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        Secrets { values, needles }
    }

    /// Also hide the `Basic` credentials made from this secret: the base64 of `user:value`. A server that echoes
    /// the `Authorization` header would show them.
    pub fn add_basic(&mut self, user: &str, name: &str) {
        let Some(v) = self.values.get(name).filter(|v| v.len() >= MIN_VALUE_LEN) else { return };
        let pair = format!("{user}:{v}");
        let tag = format!("[secret:{name}]");
        for f in [STANDARD.encode(&pair), URL_SAFE.encode(&pair), URL_SAFE_NO_PAD.encode(&pair)] {
            if !self.needles.iter().any(|(n, _)| *n == f) {
                self.needles.push((f, tag.clone()));
            }
        }
        self.needles.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// The values by name, for `secret('NAME')`.
    pub fn to_map(&self) -> HashMap<String, String> {
        self.values.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn redact_str(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (needle, tag) in &self.needles {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), tag);
            }
        }
        out
    }

    pub fn redact(&self, v: &mut Value) {
        if self.needles.is_empty() {
            return;
        }
        match v {
            Value::String(s) => {
                let r = self.redact_str(s);
                if r != *s {
                    *s = r;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.redact(x)),
            Value::Object(m) => {
                // an object key can hold a secret too
                if m.keys().any(|k| self.needles.iter().any(|(n, _)| k.contains(n.as_str()))) {
                    let old = std::mem::take(m);
                    for (k, x) in old {
                        m.insert(self.redact_str(&k), x);
                    }
                }
                m.values_mut().for_each(|x| self.redact(x));
            }
            _ => {}
        }
    }

    pub fn redact_map(&self, m: &mut Map<String, Value>) {
        if self.needles.is_empty() {
            return;
        }
        let mut v = Value::Object(std::mem::take(m));
        self.redact(&mut v);
        if let Value::Object(o) = v {
            *m = o;
        }
    }
}

/// The secret names that a workflow can use: `secret('NAME')` in any param, and the `secret` of an `auth` block.
pub fn names_used(def: &WorkflowDefinition) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for n in &def.nodes {
        let params = Value::Object(n.params.clone());
        for (_, text) in expression_strings(&params, "params") {
            out.extend(ExprEngine::secret_names(text));
        }
        if let Some(name) = n.params.get("auth").and_then(|a| a.get("secret")).and_then(Value::as_str) {
            if valid_name(name) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// The `(user, secret name)` pairs of HTTP Request nodes with basic auth and a plain-text user.
pub fn basic_auth_pairs(def: &WorkflowDefinition) -> Vec<(String, String)> {
    def.nodes
        .iter()
        .filter(|n| n.type_id == "http.request")
        .filter_map(|n| {
            let a = n.params.get("auth")?;
            (a.get("type")?.as_str()? == "basic").then_some(())?;
            Some((a.get("user").and_then(Value::as_str).unwrap_or("").to_string(), a.get("secret")?.as_str()?.to_string()))
        })
        .collect()
}

/// Read and open the named secrets. A name that does not exist is left out (`secret_not_found` at use).
pub async fn load(st: &AppState, names: &BTreeSet<String>) -> Result<Secrets, sqlx::Error> {
    if names.is_empty() {
        return Ok(Secrets::default());
    }
    let list: Vec<String> = names.iter().cloned().collect();
    let rows: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT name, value_enc FROM automation_secrets WHERE name = ANY($1)").bind(&list).fetch_all(&st.pool).await?;
    let mut values = BTreeMap::new();
    for (name, packed) in rows {
        match open(&st.crypto, &name, &packed) {
            Ok(v) => {
                values.insert(name, v);
            }
            Err(e) => tracing::error!(secret = %name, error = %e, "an automation secret can not be opened"),
        }
    }
    Ok(Secrets::new(values))
}

// ---------- routes ----------

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_secrets, put_secret)).routes(routes!(delete_secret)).route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

#[derive(Serialize, ToSchema)]
pub struct SecretInfo {
    pub name: String,
    /// The last 4 characters of the value.
    pub last4: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct SecretList {
    pub secrets: Vec<SecretInfo>,
}

#[derive(Deserialize, ToSchema)]
pub struct PutSecret {
    /// `^[A-Z][A-Z0-9_]{0,63}$`
    pub name: String,
    /// 4 to 10000 characters. Never given back.
    pub value: String,
}

/// The names of the secrets. Values are never given back.
#[utoipa::path(get, path = "/v1/automations/secrets", tag = "automations",
    responses((status = 200, body = SecretList)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Members can read the names, to use them in a workflow.")]
pub async fn list_secrets(State(st): State<AppState>) -> ApiResult<ApiJson<SecretList>> {
    let rows: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as("SELECT name, last4, updated_at FROM automation_secrets ORDER BY name").fetch_all(&st.pool).await?;
    Ok(ApiJson(SecretList { secrets: rows.into_iter().map(|(name, last4, updated_at)| SecretInfo { name, last4, updated_at }).collect() }))
}

/// Add a secret, or replace the value of one.
#[utoipa::path(post, path = "/v1/automations/secrets", tag = "automations",
    request_body = PutSecret,
    responses((status = 200, body = SecretInfo), (status = 403, description = "admin_only"), (status = 422, description = "invalid_secret")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`, and an owner/admin session or the `admin` key. The value is sealed with AES-256-GCM.")]
pub async fn put_secret(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<PutSecret>,
) -> ApiResult<ApiJson<SecretInfo>> {
    let c = caller(&session, &key);
    if !c.is_admin {
        return Err(ApiError::forbidden("admin_only", "Only an owner or an admin can add or delete secrets."));
    }
    if !valid_name(&b.name) {
        return Err(ApiError::unprocessable("invalid_secret", "The name must be capital letters, digits and `_`, start with a letter, and be 1 to 64 characters (for example SEARCH_API_KEY)."));
    }
    let len = b.value.chars().count();
    if !(MIN_VALUE_LEN..=MAX_VALUE_LEN).contains(&len) {
        return Err(ApiError::unprocessable("invalid_secret", format!("The value must be {MIN_VALUE_LEN} to {MAX_VALUE_LEN} characters.")));
    }
    let packed = seal(&st.crypto, &b.name, &b.value).map_err(|e| {
        tracing::error!(error = %e, "could not seal an automation secret");
        ApiError::internal()
    })?;
    let last4: String = b.value.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    let now = st.now();
    let row: (String, String, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO automation_secrets (name, value_enc, last4, created_by, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $5) \
         ON CONFLICT (name) DO UPDATE SET value_enc = $2, last4 = $3, updated_at = $5 RETURNING name, last4, updated_at",
    )
    .bind(&b.name)
    .bind(packed)
    .bind(&last4)
    .bind(c.user_id)
    .bind(now)
    .fetch_one(&st.pool)
    .await?;
    Ok(ApiJson(SecretInfo { name: row.0, last4: row.1, updated_at: row.2 }))
}

/// Delete a secret. Workflows that use it show `unknown_secret`.
#[utoipa::path(delete, path = "/v1/automations/secrets/{name}", tag = "automations",
    params(("name" = String, Path)),
    responses((status = 204, description = "Deleted"), (status = 403, description = "admin_only"), (status = 404, description = "secret_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`, and an owner/admin session or the `admin` key.")]
pub async fn delete_secret(State(st): State<AppState>, session: Option<Extension<SessionCtx>>, key: Option<Extension<KeyScope>>, ApiPath(name): ApiPath<String>) -> ApiResult<StatusCode> {
    if !caller(&session, &key).is_admin {
        return Err(ApiError::forbidden("admin_only", "Only an owner or an admin can add or delete secrets."));
    }
    let n = sqlx::query("DELETE FROM automation_secrets WHERE name = $1").bind(&name).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::not_found("secret_not_found", "There is no secret with this name."));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secrets() -> Secrets {
        Secrets::new(BTreeMap::from([("SEARCH_API_KEY".to_string(), "s3cr3t/Key+1".to_string()), ("TINY".to_string(), "ab".to_string())]))
    }

    #[test]
    fn names() {
        for ok in ["A", "SEARCH_API_KEY", "K9_X"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "search", "9KEY", "A-B", "A B", &"A".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn sealing_binds_the_value_to_its_name() {
        let c = Crypto::from_key_bytes(&[9u8; 32]).unwrap();
        let packed = seal(&c, "SEARCH_API_KEY", "s3cr3t-value").unwrap();
        assert!(!String::from_utf8_lossy(&packed).contains("s3cr3t"));
        assert_eq!(open(&c, "SEARCH_API_KEY", &packed).unwrap(), "s3cr3t-value");
        // copied to another name: it does not open
        assert!(open(&c, "OTHER", &packed).is_err());
        assert!(open(&c, "SEARCH_API_KEY", &packed[..10]).is_err());
        // another key does not open it
        assert!(open(&Crypto::from_key_bytes(&[1u8; 32]).unwrap(), "SEARCH_API_KEY", &packed).is_err());
    }

    #[test]
    fn the_value_and_its_encoded_forms_are_hidden() {
        let s = secrets();
        let v = "s3cr3t/Key+1";
        let forms = [
            v.to_string(),
            STANDARD.encode(v),
            URL_SAFE.encode(v),
            URL_SAFE_NO_PAD.encode(v),
            crate::automations::internal_call::urlencode(v),
        ];
        for f in forms {
            let r = s.redact_str(&format!("x {f} y"));
            assert_eq!(r, "x [secret:SEARCH_API_KEY] y", "{f}");
        }
        // Basic auth: the base64 of `user:value` holds the value in a form we can not match, so the header
        // is never stored (HTTP Request does not store request headers). The plain value in a body is hidden.
        assert_eq!(s.redact_str("nothing here"), "nothing here");
        // a value shorter than 4 characters is not used (it would hide too much)
        assert_eq!(s.redact_str("ab ab"), "ab ab");
    }

    #[test]
    fn basic_credentials_are_hidden_too() {
        let mut s = Secrets::new(BTreeMap::from([("API_KEY".to_string(), "s3cr3t-value".to_string())]));
        let header = format!("Basic {}", STANDARD.encode("ana:s3cr3t-value"));
        assert!(s.redact_str(&header).contains("Basic "), "not known yet");
        assert!(s.redact_str(&header).contains("YW5h"));
        s.add_basic("ana", "API_KEY");
        assert_eq!(s.redact_str(&header), "Basic [secret:API_KEY]");
        // an unknown secret name does nothing
        s.add_basic("ana", "NOPE");
    }

    #[test]
    fn redaction_walks_json_and_keys() {
        let s = secrets();
        let mut v = json!({ "a": "token s3cr3t/Key+1", "list": ["s3cr3t/Key+1", 3, null], "s3cr3t/Key+1": { "deep": "x s3cr3t/Key+1" } });
        s.redact(&mut v);
        assert_eq!(v, json!({ "a": "token [secret:SEARCH_API_KEY]", "list": ["[secret:SEARCH_API_KEY]", 3, null], "[secret:SEARCH_API_KEY]": { "deep": "x [secret:SEARCH_API_KEY]" } }));
        let mut m = json!({ "k": "s3cr3t/Key+1" }).as_object().cloned().unwrap();
        s.redact_map(&mut m);
        assert_eq!(m["k"], "[secret:SEARCH_API_KEY]");
    }

    #[test]
    fn names_used_finds_expressions_and_auth_blocks() {
        let def: WorkflowDefinition = serde_json::from_value(json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "nodes": [
                { "id": "a", "type": "trigger.manual" },
                { "id": "b", "type": "http.request", "params": { "url": "https://x.test", "headers": { "X-Key": "${{ secret('HEADER_KEY') }}" }, "auth": { "type": "bearer", "secret": "BEARER_KEY" } } }
            ],
            "edges": [{ "from": "a", "to": "b" }]
        }))
        .unwrap();
        assert_eq!(names_used(&def).into_iter().collect::<Vec<_>>(), vec!["BEARER_KEY", "HEADER_KEY"]);
    }
}
