//! The built-in templates (build map section 14.4 and appendix C). They are plain workflow files in
//! `src/automations/templates/`, compiled into the binary. Creating a workflow from a template goes
//! through the same import code as a file (`transfer::prepare`), so variables, providers and secrets
//! are mapped and reported the same way.
use serde_json::Value;
use std::sync::LazyLock;

pub struct Template {
    /// The id in the URL: the file name without `.aikido-workflow.json`.
    pub id: &'static str,
    pub source: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template { id: "research-lead-write-email", source: include_str!("templates/research-lead-write-email.aikido-workflow.json") },
    Template { id: "tag-hot-replies", source: include_str!("templates/tag-hot-replies.aikido-workflow.json") },
    Template { id: "website-form-to-lead", source: include_str!("templates/website-form-to-lead.aikido-workflow.json") },
    Template { id: "daily-reply-digest-to-slack", source: include_str!("templates/daily-reply-digest-to-slack.aikido-workflow.json") },
];

static PARSED: LazyLock<Vec<(&'static str, Value)>> = LazyLock::new(|| {
    TEMPLATES.iter().map(|t| (t.id, serde_json::from_str(t.source).unwrap_or_else(|e| panic!("template {} is not JSON: {e}", t.id)))).collect()
});

/// The definition of a template, as written in the file.
pub fn get(id: &str) -> Option<&'static Value> {
    PARSED.iter().find(|(i, _)| *i == id).map(|(_, v)| v)
}

pub fn all() -> impl Iterator<Item = (&'static str, &'static Value)> {
    PARSED.iter().map(|(i, v)| (*i, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::validate::{Mode, Options, validate_value};

    #[test]
    fn there_are_four_templates() {
        assert_eq!(all().count(), 4);
        for (id, def) in all() {
            assert_eq!(def["format"], "aikido-workflow", "{id}");
            assert!(def["name"].as_str().is_some_and(|n| !n.is_empty()), "{id}");
        }
    }

    #[test]
    fn every_template_passes_the_validator() {
        for (id, def) in all() {
            let r = validate_value(def, &Options { mode: Mode::Draft, ..Options::default() });
            assert!(r.ok, "template {id}: {:#?}", r.errors);
            // the only thing a template may warn about is that it has no node positions
            assert!(r.warnings.iter().all(|w| w.code == "no_positions"), "template {id}: {:#?}", r.warnings);
        }
    }
}
