//! The workflow file format (build map section 4). One type for the draft in the database, a
//! published version, the download file, the upload file and the copilot output.
//! The field order of the structs is the order of the export file.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const FORMAT: &str = "aikido-workflow";
/// The format version this server writes and reads. A newer file is refused (`format_too_new`).
pub const FORMAT_VERSION: u32 = 1;

pub const MAX_NODES: usize = 200;
pub const MAX_VARIABLES: usize = 50;
pub const MAX_NOTES: usize = 50;
pub const MAX_TRIGGERS: usize = 5;

/// Words that a node id may not be, because an expression reads them.
pub const RESERVED_IDS: &[&str] = &["trigger", "json", "item", "items", "nodes", "vars", "run", "workflow", "now", "secret"];

/// The top-level keys of a definition. Other keys are ignored with a warning.
pub const TOP_LEVEL_KEYS: &[&str] = &["format", "version", "name", "description", "variables", "settings", "nodes", "edges", "notes", "pins", "meta"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowDefinition {
    /// Always `aikido-workflow`.
    pub format: String,
    /// Format version. `1` now.
    pub version: u32,
    /// 1 to 120 characters.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub settings: Settings,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Sticky notes on the canvas. They never run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<StickyNote>,
    /// Saved sample output per node id. Test data. Not exported unless asked.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, Vec<Map<String, Value>>>,
    /// Free object. The server writes `exported_at`, `app_version` and `requires`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    /// `^[a-z][a-z0-9_]{0,39}$`, unique, not a reserved word.
    pub id: String,
    /// A type from the node catalog, for example `http.request`.
    #[serde(rename = "type")]
    pub type_id: String,
    /// Label on the canvas. Up to 80 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Checked against the schema of the type. Any string value can hold `${{ }}` expressions.
    #[serde(default)]
    pub params: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<OnError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Batch>,
    /// `true`: the node passes its input items through unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// Text shown under the node. Up to 500 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    Stop,
    Continue,
    ErrorPort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Retry {
    /// 0 to 5.
    #[serde(default)]
    pub max: u32,
    /// 100 to 60000.
    #[serde(default = "default_retry_delay")]
    pub delay_ms: u64,
}

fn default_retry_delay() -> u64 {
    1000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Batch {
    /// 1 to 100.
    #[serde(default = "default_batch_size")]
    pub size: u32,
    /// 0 to 600000.
    #[serde(default)]
    pub delay_ms: u64,
}

fn default_batch_size() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    /// A node id.
    pub from: String,
    /// An output port of `from`. Default `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// A node id. Not a trigger.
    pub to: String,
}

impl Edge {
    pub fn port(&self) -> &str {
        self.port.as_deref().unwrap_or("main")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StickyNote {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VarKind {
    Text,
    Number,
    Boolean,
    Campaign,
    List,
    Sequence,
    Account,
    Provider,
    CustomField,
    Json,
}

impl VarKind {
    /// Id kinds differ per install. On export their value is written as `null`.
    pub fn is_install_specific(self) -> bool {
        !matches!(self, VarKind::Text | VarKind::Number | VarKind::Boolean | VarKind::Json)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Variable {
    /// `^[a-z][a-z0-9_]{0,39}$`, unique. Read with `${{ vars.<key> }}`.
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub kind: VarKind,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaveData {
    All,
    ErrorsOnly,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Settings {
    /// IANA name. Default: the sending default time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Default `on_error` of nodes: `stop` (default) or `continue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<OnError>,
    /// 1 to 50. Default 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_runs: Option<u32>,
    /// 1 to 100000. Default 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs_per_hour: Option<u32>,
    /// 60 to 86400. Default 3600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_timeout_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_data: Option<SaveData>,
    /// Later (phase 9). Accepted and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_workflow_id: Option<String>,
}

impl Settings {
    pub fn on_error(&self) -> OnError {
        self.on_error.unwrap_or(OnError::Stop)
    }
    pub fn max_concurrent_runs(&self) -> u32 {
        self.max_concurrent_runs.unwrap_or(5)
    }
    pub fn max_runs_per_hour(&self) -> u32 {
        self.max_runs_per_hour.unwrap_or(1000)
    }
    pub fn run_timeout_secs(&self) -> u32 {
        self.run_timeout_secs.unwrap_or(3600)
    }
    pub fn save_data(&self) -> SaveData {
        self.save_data.unwrap_or(SaveData::All)
    }
}

impl WorkflowDefinition {
    /// A new draft: one manual trigger and nothing else.
    pub fn blank(name: &str) -> Self {
        WorkflowDefinition {
            format: FORMAT.into(),
            version: FORMAT_VERSION,
            name: name.into(),
            description: String::new(),
            variables: vec![],
            settings: Settings::default(),
            nodes: vec![Node {
                id: "start".into(),
                type_id: "trigger.manual".into(),
                name: Some("Manual trigger".into()),
                params: Map::new(),
                on_error: None,
                retry: None,
                batch: None,
                disabled: false,
                position: None,
                notes: None,
            }],
            edges: vec![],
            notes: vec![],
            pins: BTreeMap::new(),
            meta: None,
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("a definition is always valid JSON")
    }
}

/// Is this a valid node id or variable key? `^[a-z][a-z0-9_]{0,39}$`
pub fn is_slug(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z')) && s.len() <= 40 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// JSON with the keys of every object in sorted order and no spaces. The same data always gives the same text.
pub fn canonical_json(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<&String, Value> = m.iter().map(|(k, v)| (k, sort(v))).collect();
                Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    // `serde_json::Map` keeps insertion order only with the `preserve_order` feature; a BTreeMap
    // is sorted either way, so the text is stable in both cases.
    sort(v).to_string()
}

/// SHA-256 of the canonical JSON, as lower-case hex. The editor sends it back as `if_hash`.
pub fn draft_hash(v: &Value) -> String {
    Sha256::digest(canonical_json(v).as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_ignores_key_order() {
        let a = json!({ "b": 1, "a": { "y": [1, 2], "x": null } });
        let b = json!({ "a": { "x": null, "y": [1, 2] }, "b": 1 });
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(draft_hash(&a), draft_hash(&b));
        assert_ne!(draft_hash(&a), draft_hash(&json!({ "a": 1 })));
        assert_eq!(draft_hash(&a).len(), 64);
    }

    #[test]
    fn slugs() {
        assert!(is_slug("fetch_site"));
        assert!(is_slug("a"));
        assert!(!is_slug("Fetch"));
        assert!(!is_slug("1abc"));
        assert!(!is_slug("a-b"));
        assert!(!is_slug(""));
        assert!(!is_slug(&"a".repeat(41)));
        assert!(is_slug(&"a".repeat(40)));
    }

    #[test]
    fn a_blank_definition_round_trips() {
        let d = WorkflowDefinition::blank("Test");
        let back: WorkflowDefinition = serde_json::from_value(d.to_value()).unwrap();
        assert_eq!(d, back);
        assert_eq!(d.nodes[0].type_id, "trigger.manual");
    }

    #[test]
    fn the_export_key_order_is_fixed() {
        let mut d = WorkflowDefinition::blank("Test");
        d.description = "x".into();
        d.variables.push(Variable { key: "k".into(), label: String::new(), kind: VarKind::Text, value: None, required: false, description: String::new() });
        d.notes.push(StickyNote { id: "n".into(), text: "t".into(), position: None, size: None });
        let text = serde_json::to_string(&d).unwrap();
        let order = ["\"format\"", "\"version\"", "\"name\"", "\"description\"", "\"variables\"", "\"settings\"", "\"nodes\"", "\"edges\"", "\"notes\""];
        let at: Vec<usize> = order.iter().map(|k| text.find(k).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
    }
}
