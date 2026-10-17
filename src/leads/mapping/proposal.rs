//! The types of a mapping proposal (the API output) and of the confirmed mapping.
use super::targets::is_builtin;
use crate::leads::model::FieldKind;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

fn text_kind() -> FieldKind {
    FieldKind::Text
}

/// Where a column goes.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MapTarget {
    /// A built-in lead column (`email`, `first_name`, `company_name`, `tags`, ...).
    Builtin { key: String },
    /// A custom field that exists.
    Custom { key: String },
    /// A custom field that is made when the import is confirmed.
    NewField {
        key: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default = "text_kind")]
        field_kind: FieldKind,
    },
    /// Do not import this column.
    Skip,
}

impl MapTarget {
    /// `email`, `company_name`, `custom:<key>`, `ignore`.
    pub fn from_string(t: &str) -> Self {
        match t {
            "ignore" => MapTarget::Skip,
            t if t.starts_with("custom:") => MapTarget::Custom { key: t["custom:".len()..].to_string() },
            t => MapTarget::Builtin { key: t.to_string() },
        }
    }

    /// The target string the import worker reads.
    pub fn worker_string(&self, transform: Option<&str>) -> String {
        match self {
            MapTarget::Builtin { key } if key == "first_name" && transform == Some(SPLIT_FULL_NAME) => "full_name".into(),
            MapTarget::Builtin { key } => key.clone(),
            MapTarget::Custom { key } | MapTarget::NewField { key, .. } => format!("custom:{key}"),
            MapTarget::Skip => "ignore".into(),
        }
    }

    /// The names this target takes. A name can be used by one column only (`tags` is shared).
    pub fn slots(&self, transform: Option<&str>) -> Vec<String> {
        match self {
            MapTarget::Skip => vec![],
            MapTarget::Builtin { key } if key == "tags" => vec![],
            MapTarget::Builtin { key } if key == "first_name" && transform == Some(SPLIT_FULL_NAME) => vec!["first_name".into(), "last_name".into()],
            MapTarget::Builtin { key } => vec![key.clone()],
            MapTarget::Custom { key } | MapTarget::NewField { key, .. } => vec![format!("custom:{key}")],
        }
    }

    pub fn is_email(&self) -> bool {
        matches!(self, MapTarget::Builtin { key } if key == "email")
    }

    pub fn is_valid_builtin(&self) -> bool {
        match self {
            MapTarget::Builtin { key } => is_builtin(key),
            _ => true,
        }
    }
}

pub const SPLIT_FULL_NAME: &str = "split_full_name";

#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct ColumnProposal {
    pub index: usize,
    pub header: String,
    /// Up to 5 values. Emails and phones are masked (setting `mapping_mask_pii`).
    pub samples: Vec<String>,
    pub empty_count: usize,
    pub target: MapTarget,
    /// `split_full_name` or `null`.
    pub transform: Option<String>,
    /// `rule`, `ai` or `template`.
    pub source: String,
    pub confidence: f64,
    pub reason: String,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default)]
pub struct AiInfo {
    pub used: bool,
    pub provider_id: Option<Uuid>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct MappingProposal {
    pub import_id: Uuid,
    pub columns: Vec<ColumnProposal>,
    /// Column indexes with a confidence under `mapping_min_confidence`.
    pub needs_check: Vec<usize>,
    /// Codes like `ai_mapping_failed`, `ai_budget_reached`, `no_ai_provider`, `columns_over_100`, `no_email_column`, `duplicate_target:<index>`.
    pub warnings: Vec<String>,
    /// The saved mapping that was used (no AI call).
    pub template_used: Option<Uuid>,
    pub ai: AiInfo,
}

/// One column of a saved or confirmed mapping.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, PartialEq)]
pub struct TemplateColumn {
    pub index: usize,
    pub target: MapTarget,
    #[serde(default)]
    pub transform: Option<String>,
}
