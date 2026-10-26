//! Export and import of a workflow file (build map section 14). Pure code: no database, no network.
//! The routes (`transfer_routes.rs`) load the names of this install, call these functions and save the result.
//!
//! Export turns the ids of this install into names, so the file can be used on another install.
//! Import does the opposite and fixes what an LLM or a person often gets wrong. Every fix is written
//! in the report. Nothing here throws a file away: a broken file still becomes a draft with errors.
use super::definition::*;
use super::expr::{ExprEngine, expression_strings, slug};
use super::registry::Registry;
use super::schema_kit::has_expression;
use super::secrets::valid_name;
use chrono::{DateTime, SecondsFormat, Utc};
use regex::{Captures, Regex};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;
use utoipa::ToSchema;
use uuid::Uuid;

/// The biggest file we read (an upload, pasted text or a URL).
pub const MAX_IMPORT_BYTES: usize = 2 * 1024 * 1024;

// ---------- the names of this install ----------

/// What this install has. Export turns ids into names with it, import turns names into ids.
/// Each pair is `(id, name)`. Custom fields have no id: their key is the name.
#[derive(Debug, Clone, Default)]
pub struct InstallNames {
    pub campaigns: Vec<(String, String)>,
    pub lists: Vec<(String, String)>,
    pub sequences: Vec<(String, String)>,
    /// `(id, email)`
    pub accounts: Vec<(String, String)>,
    /// `(id, name)` of the AI providers.
    pub providers: Vec<(String, String)>,
    pub custom_fields: Vec<String>,
    /// The names of the secrets, never the values.
    pub secrets: Vec<String>,
    /// Installed apps, for example `reoon`.
    pub apps: Vec<String>,
}

impl InstallNames {
    fn pairs(&self, kind: VarKind) -> Vec<(String, String)> {
        match kind {
            VarKind::Campaign => self.campaigns.clone(),
            VarKind::List => self.lists.clone(),
            VarKind::Sequence => self.sequences.clone(),
            VarKind::Account => self.accounts.clone(),
            VarKind::Provider => self.providers.clone(),
            VarKind::CustomField => self.custom_fields.iter().map(|k| (k.clone(), k.clone())).collect(),
            _ => vec![],
        }
    }
    pub fn has_id(&self, kind: VarKind, id: &str) -> bool {
        self.pairs(kind).iter().any(|(i, _)| i.eq_ignore_ascii_case(id))
    }
    pub fn name_for_id(&self, kind: VarKind, id: &str) -> Option<String> {
        self.pairs(kind).into_iter().find(|(i, _)| i.eq_ignore_ascii_case(id)).map(|(_, n)| n)
    }
    /// The id of the one item with this name (case does not matter). `Err(n)`: n items have it.
    pub fn id_for_name(&self, kind: VarKind, name: &str) -> Result<Option<String>, usize> {
        let name = name.trim();
        let hits: Vec<String> = self.pairs(kind).into_iter().filter(|(_, n)| n.trim().eq_ignore_ascii_case(name)).map(|(i, _)| i).collect();
        match hits.len() {
            0 => Ok(None),
            1 => Ok(hits.into_iter().next()),
            n => Err(n),
        }
    }
    fn has_secret(&self, name: &str) -> bool {
        self.secrets.iter().any(|s| s == name)
    }
    fn has_app(&self, app: &str) -> bool {
        self.apps.iter().any(|a| a == app)
    }
}

pub fn kind_label(kind: VarKind) -> &'static str {
    match kind {
        VarKind::Text => "text",
        VarKind::Number => "number",
        VarKind::Boolean => "true/false",
        VarKind::Campaign => "campaign",
        VarKind::List => "list",
        VarKind::Sequence => "sequence",
        VarKind::Account => "sending account",
        VarKind::Provider => "AI provider",
        VarKind::CustomField => "custom field",
        VarKind::Json => "JSON",
    }
}

/// The app a node type needs, if any.
pub fn app_for_node(type_id: &str) -> Option<&'static str> {
    match type_id {
        "aikido.verify.reoon" => Some("reoon"),
        _ => None,
    }
}

// ---------- reading names out of nodes ----------

/// The secret names a list of nodes uses: `secret('NAME')` in any param, the `secret` param of a webhook
/// or an auth block, and `auth.secret`. Works on raw JSON, so it also works on a broken file.
pub fn secret_names<'a>(params: impl IntoIterator<Item = &'a Map<String, Value>>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for p in params {
        let v = Value::Object(p.clone());
        for (_, text) in expression_strings(&v, "params") {
            out.extend(ExprEngine::secret_names(text));
        }
        for cand in [p.get("secret"), p.get("auth").and_then(|a| a.get("secret"))] {
            if let Some(name) = cand.and_then(Value::as_str).filter(|s| !has_expression(s) && valid_name(s)) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

fn plain_provider(node: &Node) -> Option<&str> {
    if node.type_id != "ai.agent" {
        return None;
    }
    node.params.get("provider").and_then(Value::as_str).filter(|p| !has_expression(p))
}

// ---------- export ----------

pub struct ExportOptions<'a> {
    /// Pins can hold lead data. The editor asks first.
    pub include_pins: bool,
    pub exported_at: DateTime<Utc>,
    /// The number of the published version that is exported. `None`: the draft.
    pub from_version: Option<i32>,
    pub names: &'a InstallNames,
}

/// The file name: `<slug of the name>.aikido-workflow.json`.
pub fn export_filename(name: &str) -> String {
    let s: String = slug(name).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(80).collect();
    format!("{}.aikido-workflow.json", if s.is_empty() { "workflow" } else { s.trim_end_matches('-') })
}

/// The file text: pretty printed, keys in the fixed order of the struct (format, version, name, ...).
pub fn export_text(def: &WorkflowDefinition) -> String {
    let mut s = serde_json::to_string_pretty(def).expect("a definition is always valid JSON");
    s.push('\n');
    s
}

/// A copy of the definition that is safe to hand out: no pins (unless asked), provider ids as names, the values of
/// id variables as `null`, and `meta` with `exported_at`, `app_version`, `exported_from_version` and `requires`.
pub fn export_definition(def: &WorkflowDefinition, o: &ExportOptions) -> WorkflowDefinition {
    let mut out = def.clone();
    if !o.include_pins {
        out.pins.clear();
    }
    // a workflow id of this install means nothing elsewhere
    out.settings.error_workflow_id = None;
    for n in &mut out.nodes {
        if n.type_id == "ai.agent" {
            if let Some(Value::String(p)) = n.params.get("provider") {
                if uuid::Uuid::parse_str(p).is_ok() {
                    let name = o.names.name_for_id(VarKind::Provider, p).unwrap_or_else(|| "default_draft".into());
                    n.params.insert("provider".into(), Value::String(name));
                }
            }
        }
    }
    let mut var_requires = Vec::new();
    for v in &mut out.variables {
        if !v.kind.is_install_specific() {
            continue;
        }
        let hint = match &v.value {
            Some(Value::String(s)) if !s.is_empty() => o.names.name_for_id(v.kind, s).or_else(|| (v.kind == VarKind::CustomField).then(|| s.clone())),
            _ => None,
        };
        v.value = None;
        let mut r = json!({ "key": v.key, "kind": v.kind });
        if let Some(h) = hint {
            r["hint"] = json!(h);
        }
        var_requires.push(r);
    }
    let node_types: BTreeSet<&str> = out.nodes.iter().map(|n| n.type_id.as_str()).collect();
    let apps: BTreeSet<&str> = node_types.iter().filter_map(|t| app_for_node(t)).collect();
    let providers: BTreeSet<&str> = out.nodes.iter().filter_map(plain_provider).collect();
    let secrets = secret_names(out.nodes.iter().map(|n| &n.params));
    let mut meta = Map::new();
    meta.insert("exported_at".into(), json!(o.exported_at.to_rfc3339_opts(SecondsFormat::Secs, true)));
    meta.insert("app_version".into(), json!(env!("CARGO_PKG_VERSION")));
    meta.insert("exported_from_version".into(), json!(o.from_version));
    meta.insert(
        "requires".into(),
        json!({
            "node_types": node_types,
            "secrets": secrets,
            "providers": providers,
            "variables": var_requires,
            "apps": apps,
        }),
    );
    out.meta = Some(meta);
    out
}

// ---------- import: shapes ----------

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct NeedsYou {
    /// `variable`, `provider`, `secret` or `app`.
    pub kind: String,
    /// `variable`: the key of the variable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// `variable`: what the variable picks (`campaign`, `list`, `sequence`, `account`, `provider`, `custom_field`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub var_kind: Option<VarKind>,
    /// `secret`, `provider`, `app`: the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `provider`: the node that uses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// `variable`: the name the file had for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub message: String,
}

impl NeedsYou {
    fn new(kind: &str, message: String) -> Self {
        NeedsYou { kind: kind.into(), key: None, var_kind: None, name: None, node_id: None, hint: None, message }
    }
}

#[derive(Debug)]
pub enum ImportError {
    /// Not JSON. Line and column start at 1.
    InvalidJson { line: usize, column: usize, message: String },
    /// The file is a newer format than this server knows.
    FormatTooNew(u64),
    /// Valid JSON, but not one object.
    NotAnObject(&'static str),
    TooLarge,
}

pub struct ImportInput<'a> {
    pub names: &'a InstallNames,
    /// The choices of the person: variable key to the id (or custom field key) they picked.
    pub variables: &'a BTreeMap<String, Value>,
}

#[derive(Debug)]
pub struct Prepared {
    /// The fixed and mapped definition (raw JSON; the validator reads it).
    pub draft: Value,
    pub fixed: Vec<String>,
    /// Things the importer could only guess (an n8n file, for example).
    pub warnings: Vec<String>,
    pub needs_you: Vec<NeedsYou>,
    /// `exported_at`, `app_version` and `exported_from_version` of the file, when it had them.
    pub source: Option<Value>,
}

// ---------- import: text to JSON ----------

/// The part of the text inside the first markdown code fence, and the number of lines before it.
fn strip_fence(text: &str) -> Option<(&str, usize)> {
    let start = text.find("```")?;
    let after = &text[start + 3..];
    let body_start = after.find('\n').map(|i| i + 1)?;
    let body = &after[body_start..];
    let body = body.find("```").map_or(body, |end| &body[..end]);
    let lines_before = text[..start + 3 + body_start].matches('\n').count();
    Some((body, lines_before))
}

/// Read the text as JSON. A byte order mark and one markdown code fence are removed (written in `fixed`).
pub fn parse_text(text: &str, fixed: &mut Vec<String>) -> Result<Value, ImportError> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge);
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first = serde_json::from_str::<Value>(text);
    let err = match first {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    if let Some((body, lines_before)) = strip_fence(text) {
        match serde_json::from_str::<Value>(body) {
            Ok(v) => {
                fixed.push("removed the markdown code fence around the JSON".into());
                return Ok(v);
            }
            Err(e2) => {
                return Err(ImportError::InvalidJson { line: e2.line() + lines_before, column: e2.column(), message: e2.to_string() });
            }
        }
    }
    Err(ImportError::InvalidJson { line: err.line(), column: err.column(), message: err.to_string() })
}

/// Text to a prepared draft: parse, fix, map.
pub fn prepare_text(text: &str, input: &ImportInput) -> Result<Prepared, ImportError> {
    let mut fixed = Vec::new();
    let v = parse_text(text, &mut fixed)?;
    prepare(v, fixed, input)
}

// ---------- import: fix-ups ----------

static SPAN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\$\{\{.*?\}\}|\$\{%.*?%\}").expect("valid regex"));

/// Rewrite `<root>.old` and `<root>['old']` to the new name, only inside `${{ }}` and `${% %}`.
fn rewrite_refs(v: &mut Value, root: &str, map: &HashMap<String, String>) {
    if map.is_empty() {
        return;
    }
    let dot = Regex::new(&format!(r"\b{root}\.([A-Za-z0-9_]+)")).expect("valid regex");
    let bracket = Regex::new(&format!(r#"\b{root}\[\s*(?:'([^']*)'|"([^"]*)")\s*\]"#)).expect("valid regex");
    fn go(v: &mut Value, root: &str, map: &HashMap<String, String>, dot: &Regex, bracket: &Regex) {
        match v {
            Value::String(s) if s.contains("${") => {
                let out = SPAN_RE
                    .replace_all(s, |c: &Captures| {
                        let span = &c[0];
                        let span = dot.replace_all(span, |d: &Captures| match map.get(&d[1]) {
                            Some(new) => format!("{root}.{new}"),
                            None => d[0].to_string(),
                        });
                        bracket
                            .replace_all(&span, |b: &Captures| {
                                let old = b.get(1).or_else(|| b.get(2)).map_or("", |m| m.as_str());
                                match map.get(old) {
                                    Some(new) => format!("{root}.{new}"),
                                    None => b[0].to_string(),
                                }
                            })
                            .into_owned()
                    })
                    .into_owned();
                *s = out;
            }
            Value::Array(a) => a.iter_mut().for_each(|x| go(x, root, map, dot, bracket)),
            Value::Object(m) => m.values_mut().for_each(|x| go(x, root, map, dot, bracket)),
            _ => {}
        }
    }
    go(v, root, map, &dot, &bracket);
}

/// A node id or variable key from any text: `Fetch Site` gives `fetch_site`, `fetchSite` too.
pub fn slugify_id(s: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
        prev = Some(c);
    }
    let mut out = out.trim_matches('_').to_string();
    if out.is_empty() {
        out = "node".into();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out = format!("n_{out}");
    }
    out.truncate(40);
    out.trim_end_matches('_').to_string()
}

fn unique_id(base: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut i = 2;
    while used.contains(&candidate) {
        let suffix = format!("_{i}");
        let keep: String = base.chars().take(40 - suffix.len()).collect();
        candidate = format!("{keep}{suffix}");
        i += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn humanize(id: &str) -> String {
    let mut s = id.replace('_', " ");
    if let Some(first) = s.get(..1) {
        s = first.to_uppercase() + &s[1..];
    }
    s
}

fn version_of(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0).map(|f| f as u64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The hook for old files. Each step upgrades one version. Version 1 is the first, so only a missing or 0 version exists.
fn upgrade_version(top: &mut Map<String, Value>, from: u64, fixed: &mut Vec<String>) {
    let mut v = from;
    while v < u64::from(FORMAT_VERSION) {
        match v {
            0 => fixed.push("upgraded the file from format version 0 to 1".into()),
            _ => {}
        }
        v += 1;
    }
    top.insert("version".into(), json!(FORMAT_VERSION));
}

fn n8n_type(t: &str) -> Option<&'static str> {
    let short = t.strip_prefix("n8n-nodes-base.")?;
    match short {
        "manualTrigger" => Some("trigger.manual"),
        "httpRequest" => Some("http.request"),
        "noOp" => Some("flow.noop"),
        "webhook" => Some("trigger.webhook"),
        "if" => Some("flow.if"),
        "switch" => Some("flow.switch"),
        _ => None,
    }
}

fn looks_like_n8n(top: &Map<String, Value>) -> bool {
    top.get("connections").is_some_and(Value::is_object) && !top.contains_key("edges") && top.get("nodes").is_some_and(Value::is_array)
}

/// Best effort. Only a few node types map; the rest keep their n8n type (the validator names the closest AIKIDO types)
/// and the n8n parameters are kept in the node notes.
fn convert_n8n(top: &mut Map<String, Value>, fixed: &mut Vec<String>, warnings: &mut Vec<String>) {
    let src_nodes = top.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let connections = top.get("connections").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut used = HashSet::new();
    let mut id_of: HashMap<String, String> = HashMap::new();
    let mut type_of: HashMap<String, String> = HashMap::new();
    let mut nodes = Vec::new();
    for n in &src_nodes {
        let Some(o) = n.as_object() else { continue };
        let name = o.get("name").and_then(Value::as_str).unwrap_or("node");
        let id = unique_id(&slugify_id(name), &mut used);
        id_of.insert(name.to_string(), id.clone());
        let n8n_t = o.get("type").and_then(Value::as_str).unwrap_or("");
        let mapped = n8n_type(n8n_t);
        let type_id = mapped.map_or_else(|| n8n_t.to_string(), str::to_string);
        type_of.insert(name.to_string(), n8n_t.to_string());
        let pr = o.get("parameters").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut params = Map::new();
        match mapped {
            Some("http.request") => {
                if let Some(u) = pr.get("url").and_then(Value::as_str) {
                    params.insert("url".into(), json!(u));
                }
                params.insert("method".into(), json!(pr.get("method").and_then(Value::as_str).unwrap_or("GET").to_uppercase()));
            }
            Some("trigger.webhook") => {
                if let Some(p) = pr.get("path").and_then(Value::as_str) {
                    params.insert("path".into(), json!(p));
                }
                if let Some(m) = pr.get("httpMethod").and_then(Value::as_str) {
                    params.insert("method".into(), json!(m.to_uppercase()));
                }
            }
            _ => {}
        }
        let mut node = Map::new();
        node.insert("id".into(), json!(id));
        node.insert("type".into(), json!(type_id));
        node.insert("name".into(), json!(name));
        node.insert("params".into(), Value::Object(params));
        if mapped.is_none() || !pr.is_empty() {
            let mut note = format!("From n8n: {n8n_t}. Parameters: {}", Value::Object(pr));
            note = note.chars().take(500).collect();
            node.insert("notes".into(), json!(note));
        }
        if let Some(pos) = o.get("position").and_then(Value::as_array) {
            if let (Some(x), Some(y)) = (pos.first().and_then(Value::as_f64), pos.get(1).and_then(Value::as_f64)) {
                node.insert("position".into(), json!({ "x": x, "y": y }));
            }
        }
        if o.get("disabled").and_then(Value::as_bool) == Some(true) {
            node.insert("disabled".into(), json!(true));
        }
        nodes.push(Value::Object(node));
    }
    let mut edges = Vec::new();
    // in the order of the nodes, so the same file always gives the same result
    for src in src_nodes.iter().filter_map(|n| n.get("name").and_then(Value::as_str)) {
        let (Some(outs), Some(from)) = (connections.get(src), id_of.get(src)) else { continue };
        let kind = type_of.get(src).map(String::as_str).unwrap_or("");
        let mains = outs.get("main").and_then(Value::as_array).cloned().unwrap_or_default();
        for (i, targets) in mains.iter().enumerate() {
            for t in targets.as_array().into_iter().flatten() {
                let Some(to) = t.get("node").and_then(Value::as_str).and_then(|n| id_of.get(n)) else { continue };
                let port = if kind.ends_with(".if") {
                    Some(if i == 0 { "true" } else { "false" }.to_string())
                } else if kind.ends_with(".switch") {
                    Some(format!("case_{}", i + 1))
                } else if i == 0 {
                    None
                } else {
                    Some(format!("case_{}", i + 1))
                };
                let mut e = Map::new();
                e.insert("from".into(), json!(from));
                if let Some(p) = port {
                    e.insert("port".into(), json!(p));
                }
                e.insert("to".into(), json!(to));
                edges.push(Value::Object(e));
            }
        }
    }
    let name = top.get("name").cloned();
    top.clear();
    if let Some(n) = name {
        top.insert("name".into(), n);
    }
    top.insert("nodes".into(), Value::Array(nodes));
    top.insert("edges".into(), Value::Array(edges));
    fixed.push("converted an n8n workflow (nodes, positions and connections)".into());
    warnings.push("This is an n8n workflow, converted as well as possible. Only a few node types map to AIKIDO nodes; the n8n settings are in the node notes. Check every node.".into());
}

/// Run every safe fix-up on the top-level object. Errors only for what we cannot go on with.
fn fix_up(top: &mut Map<String, Value>, fixed: &mut Vec<String>, warnings: &mut Vec<String>) -> Result<(), ImportError> {
    if looks_like_n8n(top) {
        convert_n8n(top, fixed, warnings);
    }

    // version
    match top.get("version").cloned() {
        None | Some(Value::Null) => {
            top.insert("version".into(), json!(FORMAT_VERSION));
            fixed.push("added version".into());
        }
        Some(v) => match version_of(&v) {
            Some(n) if n > u64::from(FORMAT_VERSION) => return Err(ImportError::FormatTooNew(n)),
            Some(n) => {
                if n < u64::from(FORMAT_VERSION) {
                    upgrade_version(top, n, fixed);
                } else if !v.is_u64() {
                    top.insert("version".into(), json!(n));
                    fixed.push("version written as a whole number".into());
                }
            }
            None => {} // the validator reports it
        },
    }

    // format
    match top.get("format").and_then(Value::as_str).map(str::to_string) {
        None => {
            top.insert("format".into(), json!(FORMAT));
            fixed.push("added format".into());
        }
        Some(f) if f != FORMAT && f.trim().to_lowercase().replace(['_', ' '], "-") == FORMAT => {
            top.insert("format".into(), json!(FORMAT));
            fixed.push(format!("format '{f}' written as '{FORMAT}'"));
        }
        _ => {}
    }

    // name
    let name_ok = top.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim().is_empty());
    if !name_ok {
        top.insert("name".into(), json!("Imported workflow"));
        fixed.push("added a name: 'Imported workflow'".into());
    } else if let Some(n) = top.get("name").and_then(Value::as_str).filter(|n| n.chars().count() > 120) {
        let short: String = n.chars().take(120).collect();
        top.insert("name".into(), json!(short));
        fixed.push("cut the name to 120 characters".into());
    }

    // edges missing
    if !top.contains_key("edges") || top.get("edges").is_some_and(Value::is_null) {
        top.insert("edges".into(), json!([]));
        fixed.push("added an empty list of edges".into());
    }

    let reg = Registry::global();

    // node ids and names
    let mut renames: HashMap<String, String> = HashMap::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut names_to_id: HashMap<String, String> = HashMap::new();
    if let Some(nodes) = top.get_mut("nodes").and_then(Value::as_array_mut) {
        let mut used: HashSet<String> = HashSet::new();
        let mut keep = vec![false; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            if let Some(id) = n.get("id").and_then(Value::as_str) {
                if is_slug(id) && !RESERVED_IDS.contains(&id) && used.insert(id.to_string()) {
                    keep[i] = true;
                }
            }
        }
        let kept_ids: HashSet<String> = used.clone();
        let mut seen_invalid: HashSet<String> = HashSet::new();
        let mut named = 0;
        for (i, n) in nodes.iter_mut().enumerate() {
            let Some(o) = n.as_object_mut() else { continue };
            let old = o.get("id").and_then(Value::as_str).map(str::to_string);
            let label = o.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
            let type_id = o.get("type").and_then(Value::as_str).unwrap_or("").to_string();
            if !keep[i] {
                let base = match (&old, &label) {
                    (Some(o), _) if !o.trim().is_empty() => slugify_id(o),
                    (_, Some(l)) => slugify_id(l),
                    _ => slugify_id(type_id.rsplit('.').next().unwrap_or("node")),
                };
                let base = if RESERVED_IDS.contains(&base.as_str()) { format!("{base}_node").chars().take(40).collect() } else { base };
                let new = unique_id(&base, &mut used);
                match &old {
                    Some(old) if !old.trim().is_empty() => {
                        if is_slug(old) && !RESERVED_IDS.contains(&old.as_str()) {
                            fixed.push(format!("duplicate node id '{old}' renamed to '{new}'"));
                        } else if RESERVED_IDS.contains(&old.as_str()) {
                            fixed.push(format!("node id '{old}' is a reserved word; renamed to '{new}'"));
                        } else {
                            fixed.push(format!("node id '{old}' renamed to '{new}'"));
                        }
                        // only the first node with an old id owns the old name in expressions and edges
                        if !kept_ids.contains(old) && seen_invalid.insert(old.clone()) {
                            renames.insert(old.clone(), new.clone());
                        }
                    }
                    _ => fixed.push(format!("node {} had no id; added '{new}'", i + 1)),
                }
                if label.is_none() {
                    if let Some(old) = old.as_ref().filter(|o| !o.trim().is_empty() && !is_slug(o)) {
                        o.insert("name".into(), json!(old.trim()));
                        named += 1;
                    }
                }
                o.insert("id".into(), json!(new));
            }
            if o.get("name").and_then(Value::as_str).map(str::trim).is_none_or(str::is_empty) {
                let id = o.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let name = reg.spec(&type_id).map_or_else(|| humanize(&id), |s| s.label.to_string());
                o.insert("name".into(), json!(name));
                named += 1;
            }
            if let (Some(id), Some(name)) = (o.get("id").and_then(Value::as_str), o.get("name").and_then(Value::as_str)) {
                ids.insert(id.to_string());
                names_to_id.entry(name.trim().to_lowercase()).or_insert_with(|| id.to_string());
            }
        }
        if named > 0 {
            fixed.push(format!("added a name to {named} node{}", if named == 1 { "" } else { "s" }));
        }
    }

    // expressions: nodes.<old id> -> nodes.<new id>
    if !renames.is_empty() {
        if let Some(nodes) = top.get_mut("nodes").and_then(Value::as_array_mut) {
            for n in nodes {
                if let Some(p) = n.get_mut("params") {
                    rewrite_refs(p, "nodes", &renames);
                }
            }
        }
        if let Some(pins) = top.get_mut("pins").and_then(Value::as_object_mut) {
            let old: Vec<(String, Value)> = std::mem::take(pins).into_iter().collect();
            for (k, v) in old {
                pins.insert(renames.get(&k).cloned().unwrap_or(k), v);
            }
        }
    }

    // edges
    let mut by_name = 0;
    if let Some(edges) = top.get_mut("edges").and_then(Value::as_array_mut) {
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        let mut out = Vec::new();
        let mut renamed_fields = 0;
        let mut dups = 0;
        for e in edges.drain(..) {
            let mut e = match e {
                Value::Array(a) if a.len() == 2 && a.iter().all(Value::is_string) => json!({ "from": a[0], "to": a[1] }),
                other => other,
            };
            if let Some(o) = e.as_object_mut() {
                for (old, new) in [("source", "from"), ("target", "to")] {
                    if !o.contains_key(new) {
                        if let Some(v) = o.remove(old) {
                            o.insert(new.into(), v);
                            renamed_fields += 1;
                        }
                    }
                }
                if !o.contains_key("port") {
                    if let Some(h) = o.get("sourceHandle").and_then(Value::as_str).filter(|h| !matches!(*h, "main" | "output")) {
                        let h = h.to_string();
                        o.insert("port".into(), json!(h));
                    }
                }
                o.remove("sourceHandle");
                o.remove("targetHandle");
                if o.get("port").is_some_and(Value::is_null) {
                    o.remove("port");
                }
                for end in ["from", "to"] {
                    if let Some(s) = o.get(end).and_then(Value::as_str).map(str::to_string) {
                        if ids.contains(&s) {
                            continue;
                        }
                        if let Some(new) = renames.get(&s) {
                            o.insert(end.into(), json!(new));
                        } else if let Some(new) = names_to_id.get(&s.trim().to_lowercase()).cloned().or_else(|| Some(slugify_id(&s)).filter(|sl| ids.contains(sl))) {
                            o.insert(end.into(), json!(new));
                            by_name += 1;
                        }
                    }
                }
                let key = (
                    o.get("from").and_then(Value::as_str).unwrap_or("").to_string(),
                    o.get("port").and_then(Value::as_str).unwrap_or("main").to_string(),
                    o.get("to").and_then(Value::as_str).unwrap_or("").to_string(),
                );
                if !key.0.is_empty() && !key.2.is_empty() && !seen.insert(key) {
                    dups += 1;
                    continue;
                }
            }
            out.push(e);
        }
        *edges = out;
        if renamed_fields > 0 {
            fixed.push(format!("renamed source/target to from/to on {renamed_fields} edge end{}", if renamed_fields == 1 { "" } else { "s" }));
        }
        if dups > 0 {
            fixed.push(format!("removed {dups} duplicate edge{}", if dups == 1 { "" } else { "s" }));
        }
    }
    if by_name > 0 {
        fixed.push(format!("matched {by_name} edge end{} to a node by its name", if by_name == 1 { "" } else { "s" }));
    }

    // variable keys
    let mut var_renames: HashMap<String, String> = HashMap::new();
    if let Some(vars) = top.get_mut("variables").and_then(Value::as_array_mut) {
        let mut used: HashSet<String> = vars.iter().filter_map(|v| v.get("key").and_then(Value::as_str)).filter(|k| is_slug(k)).map(str::to_string).collect();
        for v in vars.iter_mut() {
            let Some(o) = v.as_object_mut() else { continue };
            let Some(key) = o.get("key").and_then(Value::as_str).map(str::to_string) else { continue };
            if !is_slug(&key) {
                let new = unique_id(&slugify_id(&key), &mut used);
                fixed.push(format!("variable key '{key}' renamed to '{new}'"));
                var_renames.entry(key).or_insert_with(|| new.clone());
                o.insert("key".into(), json!(new));
            }
        }
    }
    if !var_renames.is_empty() {
        if let Some(nodes) = top.get_mut("nodes").and_then(Value::as_array_mut) {
            for n in nodes {
                if let Some(p) = n.get_mut("params") {
                    rewrite_refs(p, "vars", &var_renames);
                }
            }
        }
    }
    Ok(())
}

// ---------- import: map the install-specific parts ----------

fn map_install(top: &mut Map<String, Value>, input: &ImportInput, fixed: &mut Vec<String>, needs: &mut Vec<NeedsYou>) {
    let names = input.names;
    // hints from the file
    let mut hints: HashMap<String, String> = HashMap::new();
    if let Some(list) = top.get("meta").and_then(|m| m.get("requires")).and_then(|r| r.get("variables")).and_then(Value::as_array) {
        for r in list {
            if let (Some(k), Some(h)) = (r.get("key").and_then(Value::as_str), r.get("hint").and_then(Value::as_str)) {
                hints.insert(k.to_string(), h.to_string());
            }
        }
    }

    // variables
    if let Some(vars) = top.get_mut("variables").and_then(Value::as_array_mut) {
        for v in vars.iter_mut() {
            let Some(o) = v.as_object_mut() else { continue };
            let Some(kind) = o.get("kind").and_then(|k| serde_json::from_value::<VarKind>(k.clone()).ok()) else { continue };
            if !kind.is_install_specific() {
                continue;
            }
            let Some(key) = o.get("key").and_then(Value::as_str).map(str::to_string) else { continue };
            let label = kind_label(kind);
            let current = o.get("value").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
            let chosen = input.variables.get(&key).and_then(Value::as_str).map(str::to_string);
            let mut hint = hints.get(&key).cloned();
            let mut value: Option<String> = None;
            let mut problem: Option<String> = None;
            if let Some(c) = chosen {
                if names.has_id(kind, &c) {
                    fixed.push(format!("variable '{key}' set to your choice"));
                    value = Some(c);
                } else {
                    problem = Some(format!("Your choice for '{key}' is not a {label} of this install."));
                }
            }
            if value.is_none() {
                if let Some(c) = &current {
                    if names.has_id(kind, c) {
                        value = Some(c.clone());
                    } else if hint.is_none() && Uuid::parse_str(c).is_err() {
                        hint = Some(c.clone());
                    }
                }
            }
            if value.is_none() {
                if let Some(h) = &hint {
                    match names.id_for_name(kind, h) {
                        Ok(Some(id)) => {
                            fixed.push(format!("variable '{key}' matched to the {label} '{h}' by name"));
                            value = Some(id);
                        }
                        Ok(None) => {}
                        Err(n) => problem = Some(format!("{n} {label}s are named '{h}'. Pick the right one.")),
                    }
                }
            }
            match value {
                Some(id) => {
                    o.insert("value".into(), json!(id));
                }
                None => {
                    o.insert("value".into(), Value::Null);
                    let message = problem.unwrap_or_else(|| match &hint {
                        Some(h) => format!("Pick the {label} for '{key}' (the file says '{h}')."),
                        None => format!("Pick the {label} for '{key}'."),
                    });
                    let mut n = NeedsYou::new("variable", message);
                    n.key = Some(key);
                    n.var_kind = Some(kind);
                    n.hint = hint;
                    needs.push(n);
                }
            }
        }
    }

    // providers
    let mut missing_providers: BTreeSet<String> = BTreeSet::new();
    if let Some(nodes) = top.get_mut("nodes").and_then(Value::as_array_mut) {
        for n in nodes.iter_mut() {
            let Some(o) = n.as_object_mut() else { continue };
            if o.get("type").and_then(Value::as_str) != Some("ai.agent") {
                continue;
            }
            let id = o.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let Some(p) = o.get("params").and_then(|p| p.get("provider")).and_then(Value::as_str).map(str::to_string) else { continue };
            if has_expression(&p) || p == "default_draft" || p == "default_slot" {
                continue;
            }
            let replacement = if Uuid::parse_str(&p).is_ok() {
                if names.has_id(VarKind::Provider, &p) { None } else { Some(None) }
            } else {
                match names.id_for_name(VarKind::Provider, &p) {
                    Ok(Some(_)) => names.pairs(VarKind::Provider).into_iter().find(|(_, n)| n.trim().eq_ignore_ascii_case(p.trim())).map(|(_, n)| Some(n)),
                    _ => Some(None),
                }
            };
            match replacement {
                None => {}
                Some(Some(name)) => {
                    if name != p {
                        fixed.push(format!("AI provider '{p}' on node '{id}' written as '{name}'"));
                        o.get_mut("params").and_then(Value::as_object_mut).map(|m| m.insert("provider".into(), json!(name)));
                    }
                }
                Some(None) => {
                    o.get_mut("params").and_then(Value::as_object_mut).map(|m| m.insert("provider".into(), json!("default_draft")));
                    let shown = if Uuid::parse_str(&p).is_ok() { "an AI provider of another install".to_string() } else { format!("'{p}'") };
                    let mut ny = NeedsYou::new("provider", format!("Node '{id}' used {shown}, which this install does not have. It now uses default_draft. Pick another provider if you want."));
                    ny.name = Some(if Uuid::parse_str(&p).is_ok() { "(id from another install)".into() } else { p.clone() });
                    ny.node_id = Some(id);
                    if missing_providers.insert(format!("{}:{p}", ny.node_id.as_deref().unwrap_or(""))) {
                        needs.push(ny);
                    }
                }
            }
        }
    }

    // secrets
    let used = match top.get("nodes").and_then(Value::as_array) {
        Some(nodes) => {
            let params: Vec<&Map<String, Value>> = nodes.iter().filter_map(|n| n.get("params").and_then(Value::as_object)).collect();
            secret_names(params)
        }
        None => BTreeSet::new(),
    };
    for name in used {
        if !names.has_secret(&name) {
            let mut n = NeedsYou::new("secret", format!("Add the secret {name} in Automations > Secrets."));
            n.name = Some(name);
            needs.push(n);
        }
    }

    // apps
    let apps: BTreeSet<&str> = top
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|n| n.get("type").and_then(Value::as_str))
        .filter_map(app_for_node)
        .collect();
    for app in apps {
        if !names.has_app(app) {
            let mut n = NeedsYou::new("app", format!("This workflow needs the app '{app}'. Install it in Settings > Apps."));
            n.name = Some(app.into());
            needs.push(n);
        }
    }
}

/// Fix and map a parsed file. `fixed` may already hold lines from the text step.
pub fn prepare(raw: Value, mut fixed: Vec<String>, input: &ImportInput) -> Result<Prepared, ImportError> {
    let mut warnings = Vec::new();
    let mut top = match raw {
        Value::Object(m) => m,
        Value::Array(_) => return Err(ImportError::NotAnObject("The file is a list. A workflow is one JSON object.")),
        _ => return Err(ImportError::NotAnObject("The file must be one JSON object (the workflow).")),
    };
    // { "workflow": { ... } }
    if !top.contains_key("nodes") {
        if let Some(Value::Object(inner)) = top.get("workflow").cloned() {
            top = inner;
            fixed.push("took the workflow out of the { \"workflow\": ... } wrapper".into());
        }
    }
    fix_up(&mut top, &mut fixed, &mut warnings)?;
    let mut needs = Vec::new();
    map_install(&mut top, input, &mut fixed, &mut needs);
    let source = top.get("meta").and_then(Value::as_object).map(|m| {
        let mut s = Map::new();
        for k in ["exported_at", "app_version", "exported_from_version"] {
            if let Some(v) = m.get(k).filter(|v| !v.is_null()) {
                s.insert(k.into(), v.clone());
            }
        }
        Value::Object(s)
    });
    // the meta of another install means nothing here
    if top.remove("meta").is_some() {
        fixed.push("dropped meta (it describes the install that exported the file)".into());
    }
    Ok(Prepared { draft: Value::Object(top), fixed, warnings, needs_you: needs, source: source.filter(|s| s.as_object().is_some_and(|o| !o.is_empty())) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::validate::{Options, validate_value};

    const CAMPAIGN: &str = "0192f3a0-0000-7000-8000-0000000000c1";
    const PROVIDER: &str = "0192f3a0-0000-7000-8000-0000000000e1";

    fn names() -> InstallNames {
        InstallNames {
            campaigns: vec![(CAMPAIGN.into(), "Q4 SaaS founders".into()), ("0192f3a0-0000-7000-8000-0000000000c2".into(), "Other".into())],
            lists: vec![("0192f3a0-0000-7000-8000-0000000000d1".into(), "Warm".into()), ("0192f3a0-0000-7000-8000-0000000000d2".into(), "warm".into())],
            providers: vec![(PROVIDER.into(), "Claude-Main".into())],
            custom_fields: vec!["industry".into()],
            secrets: vec!["SEARCH_API_KEY".into()],
            apps: vec![],
            ..Default::default()
        }
    }

    fn run(v: Value) -> Prepared {
        run_with(v, &BTreeMap::new())
    }

    fn run_with(v: Value, vars: &BTreeMap<String, Value>) -> Prepared {
        let n = names();
        prepare(v, vec![], &ImportInput { names: &n, variables: vars }).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn base() -> Value {
        json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "variables": [{ "key": "campaign_id", "label": "Campaign", "kind": "campaign", "value": null, "required": true }],
            "nodes": [
                { "id": "start", "type": "trigger.manual", "name": "Start", "params": {} },
                { "id": "step", "type": "flow.noop", "name": "Step", "params": {} }
            ],
            "edges": [{ "from": "start", "to": "step" }]
        })
    }

    fn typed(p: &Prepared) -> WorkflowDefinition {
        serde_json::from_value(p.draft.clone()).unwrap_or_else(|e| panic!("{e}: {}", p.draft))
    }

    fn fixed_has(p: &Prepared, part: &str) -> bool {
        p.fixed.iter().any(|f| f.contains(part))
    }

    // ---------- export ----------

    fn rich_definition() -> WorkflowDefinition {
        let (_, tpl) = crate::automations::templates::all().next().unwrap();
        let mut d = typed(&run(tpl.clone()));
        d.variables[0].value = Some(json!(CAMPAIGN));
        d.nodes.iter_mut().find(|n| n.type_id == "ai.agent").unwrap().params.insert("provider".into(), json!(PROVIDER));
        d.pins.insert("fetch_site".into(), vec![json!({ "title": "Acme", "email": "ana@acme.com" }).as_object().unwrap().clone()]);
        d.settings.error_workflow_id = Some("0192f3a0-0000-7000-8000-0000000000ff".into());
        d
    }

    fn export(d: &WorkflowDefinition, pins: bool) -> WorkflowDefinition {
        let n = names();
        export_definition(d, &ExportOptions { include_pins: pins, exported_at: "2026-10-26T10:00:00Z".parse().unwrap(), from_version: Some(3), names: &n })
    }

    #[test]
    fn export_then_import_gives_the_same_definition() {
        let d = rich_definition();
        let text = export_text(&export(&d, false));
        let back = typed(&prepare_text(&text, &ImportInput { names: &names(), variables: &BTreeMap::new() }).unwrap());
        let mut expect = d.clone();
        // what export takes out or changes on purpose
        expect.pins.clear();
        expect.settings.error_workflow_id = None;
        expect.nodes.iter_mut().find(|n| n.type_id == "ai.agent").unwrap().params.insert("provider".into(), json!("Claude-Main"));
        assert_eq!(back, expect);
    }

    #[test]
    fn export_round_trip_keeps_pins_when_asked() {
        let d = rich_definition();
        let text = export_text(&export(&d, true));
        let back = typed(&prepare_text(&text, &ImportInput { names: &names(), variables: &BTreeMap::new() }).unwrap());
        assert_eq!(back.pins, d.pins);
    }

    #[test]
    fn pins_are_not_exported_by_default() {
        let d = rich_definition();
        let out = export(&d, false);
        assert!(out.pins.is_empty());
        assert!(!export_text(&out).contains("ana@acme.com"));
        assert!(!export_text(&out).contains("\"pins\""));
        assert!(export_text(&export(&d, true)).contains("ana@acme.com"));
    }

    #[test]
    fn export_writes_names_not_ids_and_the_meta() {
        let out = export(&rich_definition(), false);
        let text = export_text(&out);
        assert!(!text.contains(CAMPAIGN), "{text}");
        assert!(!text.contains(PROVIDER), "{text}");
        assert!(!text.contains("0192f3a0-0000-7000-8000-0000000000ff"), "the error workflow id stays out");
        assert!(out.variables[0].value.is_none());
        let m = out.meta.as_ref().unwrap();
        assert_eq!(m["exported_at"], "2026-10-26T10:00:00Z");
        assert_eq!(m["exported_from_version"], 3);
        assert_eq!(m["app_version"], env!("CARGO_PKG_VERSION"));
        let r = &m["requires"];
        assert_eq!(r["variables"], json!([{ "key": "campaign_id", "kind": "campaign", "hint": "Q4 SaaS founders" }]));
        assert_eq!(r["providers"], json!(["Claude-Main"]));
        assert_eq!(r["secrets"], json!(["BRAVE_SEARCH_API_KEY"]));
        assert!(r["node_types"].as_array().unwrap().iter().any(|t| t == "web.fetch_page"));
        assert_eq!(r["apps"], json!([]));
    }

    #[test]
    fn export_has_only_secret_names_and_known_keys() {
        let text = export_text(&export(&rich_definition(), true));
        let v: Value = serde_json::from_str(&text).unwrap();
        for k in v.as_object().unwrap().keys() {
            assert!(TOP_LEVEL_KEYS.contains(&k.as_str()), "{k}");
        }
        // names of secrets only: the file never has a value, a key or a user id
        assert!(!text.contains("created_by") && !text.contains("run_as") && !text.contains("api_key_ciphertext"));
        assert!(v["meta"]["requires"]["secrets"].as_array().unwrap().iter().all(|s| valid_name(s.as_str().unwrap())));
    }

    #[test]
    fn the_export_text_has_the_fixed_key_order() {
        let text = export_text(&export(&rich_definition(), false));
        let order = ["\"format\"", "\"version\"", "\"name\"", "\"description\"", "\"variables\"", "\"settings\"", "\"nodes\"", "\"edges\"", "\"meta\""];
        let at: Vec<usize> = order.iter().map(|k| text.find(k).unwrap_or_else(|| panic!("{k}"))).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn file_names() {
        assert_eq!(export_filename("Research lead & write email"), "research-lead-write-email.aikido-workflow.json");
        assert_eq!(export_filename("???"), "workflow.aikido-workflow.json");
    }

    #[test]
    fn export_marks_apps() {
        let mut d = typed(&run(base()));
        d.nodes.push(Node { id: "verify".into(), type_id: "aikido.verify.reoon".into(), name: None, params: Map::new(), on_error: None, retry: None, batch: None, disabled: false, position: None, notes: None });
        assert_eq!(export(&d, false).meta.unwrap()["requires"]["apps"], json!(["reoon"]));
    }

    // ---------- import: text ----------

    #[test]
    fn invalid_json_has_line_and_column() {
        let n = names();
        let err = prepare_text("{\n  \"format\": \"aikido-workflow\",\n  \"nodes\": [,]\n}", &ImportInput { names: &n, variables: &BTreeMap::new() }).unwrap_err();
        match err {
            ImportError::InvalidJson { line, column, .. } => {
                assert_eq!(line, 3);
                assert!(column > 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_code_fence_is_removed_and_lines_count_from_the_text() {
        let mut fixed = vec![];
        let v = parse_text("Here is your workflow:\n\n```json\n{ \"a\": 1 }\n```\nHope it helps!", &mut fixed).unwrap();
        assert_eq!(v, json!({ "a": 1 }));
        assert_eq!(fixed.len(), 1);
        let err = parse_text("Intro\n```json\n{\n  \"a\": ,\n}\n```", &mut vec![]).unwrap_err();
        match err {
            ImportError::InvalidJson { line, .. } => assert_eq!(line, 4),
            other => panic!("{other:?}"),
        }
        // a fence without a closing line works too, and a bare fence without a language
        assert!(parse_text("```\n{\"a\":1}", &mut vec![]).is_ok());
    }

    #[test]
    fn a_byte_order_mark_and_big_files() {
        assert!(parse_text("\u{feff}{\"a\":1}", &mut vec![]).is_ok());
        assert!(matches!(parse_text(&" ".repeat(MAX_IMPORT_BYTES + 1), &mut vec![]), Err(ImportError::TooLarge)));
    }

    #[test]
    fn not_an_object() {
        let n = names();
        let input = ImportInput { names: &n, variables: &BTreeMap::new() };
        assert!(matches!(prepare(json!([1]), vec![], &input), Err(ImportError::NotAnObject(_))));
        assert!(matches!(prepare(json!("x"), vec![], &input), Err(ImportError::NotAnObject(_))));
    }

    // ---------- import: fix-ups ----------

    #[test]
    fn a_workflow_wrapper_is_opened() {
        let p = run(json!({ "workflow": base() }));
        assert!(fixed_has(&p, "wrapper"));
        assert_eq!(typed(&p).nodes.len(), 2);
    }

    #[test]
    fn versions() {
        let mut v = base();
        v.as_object_mut().unwrap().remove("version");
        assert!(fixed_has(&run(v), "added version"));
        let mut v = base();
        v["version"] = json!(0);
        let p = run(v);
        assert!(fixed_has(&p, "version 0 to 1"));
        assert_eq!(p.draft["version"], 1);
        let mut v = base();
        v["version"] = json!("1");
        assert_eq!(run(v).draft["version"], 1);
        let mut v = base();
        v["version"] = json!(2);
        let n = names();
        assert!(matches!(prepare(v, vec![], &ImportInput { names: &n, variables: &BTreeMap::new() }), Err(ImportError::FormatTooNew(2))));
    }

    #[test]
    fn format_and_name_and_edges_are_added() {
        let p = run(json!({ "version": 1, "nodes": [{ "id": "start", "type": "trigger.manual" }] }));
        assert!(fixed_has(&p, "added format"));
        assert!(fixed_has(&p, "added a name"));
        assert!(fixed_has(&p, "empty list of edges"));
        assert_eq!(p.draft["format"], FORMAT);
        assert_eq!(p.draft["name"], "Imported workflow");
        assert_eq!(p.draft["edges"], json!([]));
        let mut v = base();
        v["format"] = json!("Aikido_Workflow");
        let p = run(v);
        assert_eq!(p.draft["format"], FORMAT);
        // another format name is left for the validator
        let mut v = base();
        v["format"] = json!("n8n");
        assert_eq!(run(v).draft["format"], "n8n");
    }

    #[test]
    fn ids_are_slugged_and_expressions_follow() {
        let p = run(json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "variables": [{ "key": "Campaign Id", "kind": "text", "value": "x" }],
            "nodes": [
                { "id": "On Start", "type": "trigger.manual" },
                { "id": "Fetch Site", "type": "web.fetch_page", "params": { "url": "https://acme.com" } },
                { "id": "WriteEmail", "type": "data.set", "params": { "keep": "none", "fields": {
                    "a": "${{ nodes.Fetch Site.json.text }}",
                    "b": "${{ nodes['Fetch Site'].json.text }}",
                    "c": "${{ nodes[\"Fetch Site\"].json.title | default('') }} and ${{ nodes.WriteEmail.json.x | default('') }}",
                    "d": "plain nodes['Fetch Site'] stays outside an expression",
                    "e": "${{ vars['Campaign Id'] }}"
                } } }
            ],
            "edges": [
                { "from": "On Start", "to": "Fetch Site" },
                { "source": "Fetch Site", "target": "WriteEmail" }
            ]
        }));
        let d = typed(&p);
        let ids: Vec<&str> = d.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["on_start", "fetch_site", "write_email"]);
        assert!(fixed_has(&p, "node id 'Fetch Site' renamed to 'fetch_site'"), "{:?}", p.fixed);
        assert!(fixed_has(&p, "node id 'WriteEmail' renamed to 'write_email'"));
        let f = &d.nodes[2].params["fields"];
        assert_eq!(f["b"], "${{ nodes.fetch_site.json.text }}");
        assert_eq!(f["c"], "${{ nodes.fetch_site.json.title | default('') }} and ${{ nodes.write_email.json.x | default('') }}");
        assert_eq!(f["d"], "plain nodes['Fetch Site'] stays outside an expression");
        assert_eq!(f["e"], "${{ vars.campaign_id }}");
        assert_eq!(d.variables[0].key, "campaign_id");
        assert_eq!(d.edges, vec![Edge { from: "on_start".into(), port: None, to: "fetch_site".into() }, Edge { from: "fetch_site".into(), port: None, to: "write_email".into() }]);
        // the old name becomes the label
        assert_eq!(d.nodes[1].name.as_deref(), Some("Fetch Site"));
    }

    #[test]
    fn duplicate_reserved_and_missing_ids() {
        let p = run(json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "nodes": [
                { "id": "a", "type": "trigger.manual" },
                { "id": "a", "type": "flow.noop" },
                { "id": "json", "type": "flow.noop" },
                { "type": "web.fetch_page", "params": { "url": "https://x.com" } },
                { "type": "flow.noop", "name": "Second Step" }
            ],
            "edges": [{ "from": "a", "to": "a_2" }]
        }));
        let d = typed(&p);
        let ids: Vec<&str> = d.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "a_2", "json_node", "fetch_page", "second_step"]);
        assert!(fixed_has(&p, "duplicate node id 'a' renamed to 'a_2'"));
        assert!(fixed_has(&p, "reserved word"));
        assert!(fixed_has(&p, "had no id; added 'fetch_page'"));
        // the result has no duplicate or reserved id left
        let r = validate_value(&p.draft, &Options::default());
        assert!(!r.has_code("duplicate_node_id") && !r.has_code("reserved_node_id") && !r.has_code("invalid_node_id"), "{:?}", r.errors);
    }

    #[test]
    fn slug_ids() {
        assert_eq!(slugify_id("Fetch Site"), "fetch_site");
        assert_eq!(slugify_id("fetchSite"), "fetch_site");
        assert_eq!(slugify_id("HTTP Request"), "http_request");
        assert_eq!(slugify_id("1st step"), "n_1st_step");
        assert_eq!(slugify_id("  --  "), "node");
        assert!(is_slug(&slugify_id(&"x y ".repeat(30))));
    }

    #[test]
    fn nodes_without_a_name_get_one() {
        let p = run(json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "fetch", "type": "web.fetch_page", "params": { "url": "https://x.com" } }, { "id": "my_node", "type": "nope.nope" }],
            "edges": []
        }));
        let d = typed(&p);
        assert_eq!(d.nodes[0].name.as_deref(), Some("Manual trigger"));
        assert_eq!(d.nodes[1].name.as_deref(), Some(Registry::global().spec("web.fetch_page").unwrap().label));
        assert_eq!(d.nodes[2].name.as_deref(), Some("My node"));
        assert!(fixed_has(&p, "added a name to 3 nodes"));
    }

    #[test]
    fn source_target_and_handles() {
        let p = run(json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "nodes": [{ "id": "t", "type": "trigger.manual" }, { "id": "c", "type": "flow.if", "params": { "expression": "${{ true }}" } }, { "id": "y", "type": "flow.noop" }, { "id": "n", "type": "flow.noop" }],
            "edges": [
                { "source": "t", "target": "c", "sourceHandle": "main" },
                { "source": "c", "sourceHandle": "true", "target": "y" },
                ["c", "n"],
                { "from": "c", "port": null, "to": "n" }
            ]
        }));
        let d = typed(&p);
        assert_eq!(d.edges[0], Edge { from: "t".into(), port: None, to: "c".into() });
        assert_eq!(d.edges[1], Edge { from: "c".into(), port: Some("true".into()), to: "y".into() });
        assert_eq!(d.edges[2], Edge { from: "c".into(), port: None, to: "n".into() });
        assert_eq!(d.edges.len(), 3, "the last one is a duplicate of the list form");
        assert!(fixed_has(&p, "renamed source/target to from/to"));
        assert!(fixed_has(&p, "duplicate edge"));
    }

    #[test]
    fn edges_that_name_a_node_by_its_label() {
        let p = run(json!({
            "format": "aikido-workflow", "version": 1, "name": "T",
            "nodes": [{ "id": "start", "type": "trigger.manual", "name": "Start here" }, { "id": "step", "type": "flow.noop", "name": "Do it" }],
            "edges": [{ "from": "Start here", "to": "do it" }]
        }));
        assert_eq!(typed(&p).edges, vec![Edge { from: "start".into(), port: None, to: "step".into() }]);
        assert!(fixed_has(&p, "by its name"));
    }

    #[test]
    fn duplicate_edges_are_removed() {
        let mut v = base();
        v["edges"] = json!([{ "from": "start", "to": "step" }, { "from": "start", "port": "main", "to": "step" }, { "from": "start", "to": "step" }]);
        let p = run(v);
        assert_eq!(typed(&p).edges.len(), 1);
        assert!(fixed_has(&p, "removed 2 duplicate edges"));
    }

    #[test]
    fn n8n_connections_are_converted_best_effort() {
        let p = run(json!({
            "name": "My n8n flow",
            "nodes": [
                { "name": "Manual Trigger", "type": "n8n-nodes-base.manualTrigger", "position": [100, 200], "parameters": {} },
                { "name": "Get Page", "type": "n8n-nodes-base.httpRequest", "position": [300, 200], "parameters": { "url": "https://acme.com", "method": "get" } },
                { "name": "Is OK", "type": "n8n-nodes-base.if", "parameters": {} },
                { "name": "Send Slack", "type": "n8n-nodes-base.slack", "parameters": { "text": "hi" } }
            ],
            "connections": {
                "Manual Trigger": { "main": [[{ "node": "Get Page", "type": "main", "index": 0 }]] },
                "Get Page": { "main": [[{ "node": "Is OK", "type": "main", "index": 0 }]] },
                "Is OK": { "main": [[{ "node": "Send Slack", "type": "main", "index": 0 }], []] }
            },
            "settings": { "executionOrder": "v1" }, "active": false
        }));
        assert!(fixed_has(&p, "n8n"));
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].contains("n8n"));
        let d = typed(&p);
        assert_eq!(d.name, "My n8n flow");
        assert_eq!(d.nodes[0].type_id, "trigger.manual");
        assert_eq!(d.nodes[0].position, Some(Position { x: 100.0, y: 200.0 }));
        assert_eq!(d.nodes[1].type_id, "http.request");
        assert_eq!(d.nodes[1].params["method"], "GET");
        assert_eq!(d.nodes[1].params["url"], "https://acme.com");
        assert_eq!(d.nodes[2].type_id, "flow.if");
        assert_eq!(d.nodes[3].type_id, "n8n-nodes-base.slack");
        assert!(d.nodes[3].notes.as_deref().unwrap().contains("From n8n"));
        assert_eq!(d.edges.len(), 3);
        assert_eq!(d.edges[2], Edge { from: "is_ok".into(), port: Some("true".into()), to: "send_slack".into() });
        // the unknown type is left for the validator, which names the closest types
        let r = validate_value(&p.draft, &Options::default());
        assert!(r.has_code("unknown_node_type"));
        assert!(!r.has_code("unknown_field_ignored"), "{:?}", r.warnings);
    }

    #[test]
    fn meta_is_dropped_and_reported() {
        let mut v = base();
        v["meta"] = json!({ "exported_at": "2026-10-01T00:00:00Z", "app_version": "0.9.0", "exported_from_version": null, "requires": {} });
        let p = run(v);
        assert!(p.draft.get("meta").is_none());
        assert_eq!(p.source.unwrap(), json!({ "exported_at": "2026-10-01T00:00:00Z", "app_version": "0.9.0" }));
    }

    #[test]
    fn a_broken_file_still_gives_a_draft() {
        let p = run(json!({ "nodes": [{ "id": "x", "type": "does.not.exist" }, 5], "edges": [{ "from": "x", "to": "ghost" }] }));
        let r = validate_value(&p.draft, &Options::default());
        assert!(!r.ok);
        assert!(r.has_code("unknown_node_type"));
    }

    // ---------- import: mapping ----------

    fn needs<'a>(p: &'a Prepared, kind: &str) -> Vec<&'a NeedsYou> {
        p.needs_you.iter().filter(|n| n.kind == kind).collect()
    }

    #[test]
    fn variables_by_the_choice_of_the_person() {
        let vars = BTreeMap::from([("campaign_id".to_string(), json!(CAMPAIGN))]);
        let p = run_with(base(), &vars);
        assert_eq!(typed(&p).variables[0].value, Some(json!(CAMPAIGN)));
        assert!(needs(&p, "variable").is_empty());
        // a choice that is not on this install is reported
        let vars = BTreeMap::from([("campaign_id".to_string(), json!("0192f3a0-0000-7000-8000-00000000ffff"))]);
        let p = run_with(base(), &vars);
        assert_eq!(typed(&p).variables[0].value, None);
        assert_eq!(needs(&p, "variable").len(), 1);
    }

    #[test]
    fn variables_by_the_name_hint() {
        let mut v = base();
        v["meta"] = json!({ "requires": { "variables": [{ "key": "campaign_id", "kind": "campaign", "hint": "q4 saas FOUNDERS" }] } });
        let p = run(v);
        assert_eq!(typed(&p).variables[0].value, Some(json!(CAMPAIGN)));
        assert!(fixed_has(&p, "matched to the campaign"));
        assert!(p.needs_you.is_empty());
    }

    #[test]
    fn variables_left_empty_are_listed_with_the_hint() {
        let mut v = base();
        v["meta"] = json!({ "requires": { "variables": [{ "key": "campaign_id", "kind": "campaign", "hint": "Nothing like it" }] } });
        let p = run(v);
        let n = needs(&p, "variable");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].key.as_deref(), Some("campaign_id"));
        assert_eq!(n[0].var_kind, Some(VarKind::Campaign));
        assert_eq!(n[0].hint.as_deref(), Some("Nothing like it"));
        assert!(n[0].message.contains("the file says 'Nothing like it'"), "{}", n[0].message);
        // without a hint
        let p = run(base());
        assert_eq!(needs(&p, "variable")[0].message, "Pick the campaign for 'campaign_id'.");
    }

    #[test]
    fn two_items_with_the_same_name_are_not_guessed() {
        let mut v = base();
        v["variables"] = json!([{ "key": "list_id", "kind": "list", "value": null }]);
        v["meta"] = json!({ "requires": { "variables": [{ "key": "list_id", "kind": "list", "hint": "WARM" }] } });
        let p = run(v);
        assert_eq!(typed(&p).variables[0].value, None);
        assert!(needs(&p, "variable")[0].message.contains("2 lists are named"));
    }

    #[test]
    fn plain_variables_and_known_ids_are_left_alone() {
        let mut v = base();
        v["variables"] = json!([
            { "key": "n", "kind": "number", "value": 5 },
            { "key": "campaign_id", "kind": "campaign", "value": CAMPAIGN },
            { "key": "field", "kind": "custom_field", "value": "industry" },
            { "key": "field2", "kind": "custom_field", "value": "unknown_field" }
        ]);
        let p = run(v);
        let d = typed(&p);
        assert_eq!(d.variables[0].value, Some(json!(5)));
        assert_eq!(d.variables[1].value, Some(json!(CAMPAIGN)));
        assert_eq!(d.variables[2].value, Some(json!("industry")));
        assert_eq!(d.variables[3].value, None);
        assert_eq!(needs(&p, "variable").len(), 1);
    }

    fn with_provider(p: Value) -> Value {
        let mut v = base();
        v["nodes"].as_array_mut().unwrap().push(json!({ "id": "ai", "type": "ai.agent", "name": "AI", "params": { "provider": p, "prompt": "hi" } }));
        v
    }

    #[test]
    fn providers_by_name() {
        let p = run(with_provider(json!("claude-main")));
        assert_eq!(typed(&p).nodes[2].params["provider"], "Claude-Main");
        assert!(needs(&p, "provider").is_empty());
        for keep in ["default_draft", "default_slot", "${{ vars.p }}"] {
            let p = run(with_provider(json!(keep)));
            assert_eq!(typed(&p).nodes[2].params["provider"], keep);
            assert!(needs(&p, "provider").is_empty());
        }
        // the id of this install stays
        let p = run(with_provider(json!(PROVIDER)));
        assert_eq!(typed(&p).nodes[2].params["provider"], PROVIDER);
    }

    #[test]
    fn unknown_providers_become_default_draft() {
        let p = run(with_provider(json!("gpt-elsewhere")));
        assert_eq!(typed(&p).nodes[2].params["provider"], "default_draft");
        let n = needs(&p, "provider");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].name.as_deref(), Some("gpt-elsewhere"));
        assert_eq!(n[0].node_id.as_deref(), Some("ai"));
        let p = run(with_provider(json!("0192f3a0-0000-7000-8000-00000000abcd")));
        assert_eq!(typed(&p).nodes[2].params["provider"], "default_draft");
        assert_eq!(needs(&p, "provider").len(), 1);
        // and the validator is happy with it
        let known: HashSet<String> = ["Claude-Main".to_string()].into();
        let r = validate_value(&p.draft, &Options { providers: Some(&known), ..Options::default() });
        assert!(!r.has_code("unknown_provider"), "{:?}", r.errors);
    }

    #[test]
    fn missing_secrets_and_apps_are_listed() {
        let mut v = base();
        v["nodes"].as_array_mut().unwrap().push(json!({ "id": "web", "type": "http.request", "name": "Web", "params": {
            "url": "https://api.example.com", "headers": { "X-Key": "${{ secret('OTHER_KEY') }}" }, "auth": { "type": "bearer", "secret": "SEARCH_API_KEY" } } }));
        v["nodes"].as_array_mut().unwrap().push(json!({ "id": "hook", "type": "trigger.webhook", "name": "Hook", "params": { "auth": "header", "header_name": "X-S", "secret": "HOOK_SECRET" } }));
        v["nodes"].as_array_mut().unwrap().push(json!({ "id": "verify", "type": "aikido.verify.reoon", "name": "Verify", "params": {} }));
        let p = run(v);
        let secrets: Vec<&str> = needs(&p, "secret").iter().map(|n| n.name.as_deref().unwrap()).collect();
        assert_eq!(secrets, vec!["HOOK_SECRET", "OTHER_KEY"], "SEARCH_API_KEY exists on this install");
        assert_eq!(needs(&p, "secret")[0].message, "Add the secret HOOK_SECRET in Automations > Secrets.");
        let apps = needs(&p, "app");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name.as_deref(), Some("reoon"));
    }

    #[test]
    fn the_name_override_and_serialized_report_shape() {
        let p = run(with_provider(json!("zzz")));
        let v = serde_json::to_value(&p.needs_you[0]).unwrap();
        assert_eq!(v["kind"], "variable");
        assert!(v.get("name").is_none() && v.get("node_id").is_none());
        assert!(v["message"].is_string());
    }

    #[test]
    fn every_template_imports_clean() {
        for (id, def) in crate::automations::templates::all() {
            let p = run(def.clone());
            let r = validate_value(&p.draft, &Options::default());
            assert!(r.ok, "{id}: {:?}", r.errors);
            assert!(p.warnings.is_empty(), "{id}");
        }
    }
}
