//! `workprocess.md` (build map section 15): one file that teaches any LLM to write AIKIDO workflows.
//! `docs/workprocess.md` is the hand-written part. It has one marker line, `<!-- NODE_CATALOG -->`; the
//! catalog made from the `NodeSpec`s replaces it. With the install part, the names of this install
//! (campaigns, lists, providers, custom field keys, secret names; never values) are added at the end.
//!
//! `merge` is pure: the routes and the copilot (phase 8) both call it.
use super::registry::{Category, ExecMode, NodeSpec, Registry};
use super::transfer::InstallNames;
use serde_json::Value;
use std::fmt::Write;

/// The hand-written part, from the repo.
pub const SOURCE: &str = include_str!("../../docs/workprocess.md");
pub const CATALOG_MARKER: &str = "<!-- NODE_CATALOG -->";

/// The doc with the catalog of `registry` in place of the marker, and, if `install` is given, the install part.
pub fn merge(doc: &str, registry: &Registry, install: Option<&InstallNames>) -> String {
    let mut out = doc.replace(CATALOG_MARKER, catalog_markdown(registry).trim_end());
    if let Some(i) = install {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&install_markdown(i));
    }
    out
}

/// The merged `workprocess.md` of this build.
pub fn workprocess(install: Option<&InstallNames>) -> String {
    merge(SOURCE, Registry::global(), install)
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").trim().to_string()
}

/// A short type for a params schema: `string`, `one of: a | b`, `list of string`, `{ key: type, key?: type }`.
fn type_label(schema: &Value, depth: usize) -> String {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.iter().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)).collect::<Vec<_>>().join(" | ");
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("array") => format!("list of {}", schema.get("items").map_or_else(|| "any".to_string(), |i| type_label(i, depth + 1))),
        Some("object") => match schema.get("properties").and_then(Value::as_object).filter(|p| !p.is_empty() && depth < 3) {
            Some(props) => {
                let required: Vec<&str> = schema.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let parts: Vec<String> = props.iter().map(|(k, v)| format!("{k}{}: {}", if required.contains(&k.as_str()) { "" } else { "?" }, type_label(v, depth + 1))).collect();
                format!("{{ {} }}", parts.join(", "))
            }
            None => "object".into(),
        },
        Some(t) => t.to_string(),
        None => "any".into(),
    }
}

fn node_markdown(s: &NodeSpec, out: &mut String) {
    let _ = writeln!(out, "#### `{}` ({})\n", s.type_id, s.label);
    let _ = writeln!(out, "{}\n", s.description.trim());
    let mut facts = vec![format!("Runs: {}", if s.mode == ExecMode::PerItem { "once per item" } else { "once with all items" })];
    facts.push(format!("Ports: {}", s.ports.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")));
    if s.side_effect {
        facts.push("Changes data (a test run only pretends)".into());
    }
    if s.admin_only {
        facts.push("Only an owner or admin may publish it".into());
    }
    let _ = writeln!(out, "{}\n", facts.join(". ") + ".");
    let props = s.params_schema.get("properties").and_then(Value::as_object);
    match props.filter(|p| !p.is_empty()) {
        Some(props) => {
            let required: Vec<&str> = s.params_schema.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            out.push_str("| Param | Type | Required | Notes |\n| --- | --- | --- | --- |\n");
            for (k, v) in props {
                let mut notes = v.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                if let Some(d) = v.get("default") {
                    if !notes.is_empty() {
                        notes.push(' ');
                    }
                    let _ = write!(notes, "Default: `{d}`.");
                }
                let _ = writeln!(out, "| `{k}` | {} | {} | {} |", cell(&type_label(v, 0)), if required.contains(&k.as_str()) { "yes" } else { "no" }, cell(&notes));
            }
            out.push('\n');
        }
        None => out.push_str("No params.\n\n"),
    }
    if let Some(extra) = s.params_schema.get("allOf").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let _ = extra;
        out.push_str("Some params are required only with certain values of other params (see the notes above).\n\n");
    }
    let mut example = s.output_example.to_string();
    if example.chars().count() > 700 {
        example = example.chars().take(700).collect::<String>() + "...";
    }
    let _ = writeln!(out, "Output item example: `{}`\n", example.replace('`', "'"));
    if !s.docs_notes.trim().is_empty() {
        let _ = writeln!(out, "Notes: {}\n", s.docs_notes.trim());
    }
}

/// The node catalog as Markdown, grouped by category, one entry per node type.
pub fn catalog_markdown(registry: &Registry) -> String {
    let mut out = String::new();
    let groups = [
        (Category::Trigger, "Triggers"),
        (Category::Aikido, "AIKIDO nodes (leads, campaigns, replies, sending)"),
        (Category::Ai, "AI"),
        (Category::Web, "Web and HTTP"),
        (Category::Flow, "Flow"),
        (Category::Data, "Data"),
    ];
    let specs: Vec<&NodeSpec> = registry.specs().collect();
    for (cat, title) in groups {
        let in_group: Vec<&&NodeSpec> = specs.iter().filter(|s| s.category == cat).collect();
        if in_group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {title}\n");
        for s in in_group {
            node_markdown(s, &mut out);
        }
    }
    out
}

/// "This install": the real names and ids, so an outside LLM can fill them in. Never values of secrets.
pub fn install_markdown(i: &InstallNames) -> String {
    let mut out = String::from("## 11. This install\n\nUse these real ids in `variables` values or in params when the user means one of them. Do not invent ids. Secret values are never shown here, only the names.\n\n");
    let list = |out: &mut String, title: &str, items: &[(String, String)]| {
        let _ = writeln!(out, "{title}:");
        if items.is_empty() {
            out.push_str("- (none)\n");
        }
        for (id, name) in items {
            let _ = writeln!(out, "- {} (`{id}`)", name.replace('\n', " "));
        }
        out.push('\n');
    };
    list(&mut out, "Campaigns", &i.campaigns);
    list(&mut out, "Lists", &i.lists);
    let _ = writeln!(out, "AI providers (use the name in `provider`, or `default_draft` / `default_slot`):");
    if i.providers.is_empty() {
        out.push_str("- (none)\n");
    }
    for (_, name) in &i.providers {
        let _ = writeln!(out, "- {}", name.replace('\n', " "));
    }
    out.push('\n');
    let _ = writeln!(out, "Custom lead field keys (for `aikido.lead.update` `custom_fields`): {}\n", if i.custom_fields.is_empty() { "(none)".to_string() } else { i.custom_fields.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ") });
    let _ = writeln!(out, "Secret names (use `${{{{ secret('NAME') }}}}`): {}", if i.secrets.is_empty() { "(none)".to_string() } else { i.secrets.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ") });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_replaced_once() {
        let m = workprocess(None);
        assert!(!m.contains(CATALOG_MARKER));
        assert_eq!(SOURCE.matches(CATALOG_MARKER).count(), 1);
        assert!(m.contains("#### `http.request`"));
    }

    #[test]
    fn the_install_part_has_names_and_no_secret_values() {
        let names = InstallNames {
            campaigns: vec![("0192f3a0-0000-7000-8000-0000000000c1".into(), "Q4 SaaS founders".into())],
            lists: vec![("0192f3a0-0000-7000-8000-0000000000d1".into(), "Warm leads".into())],
            providers: vec![("0192f3a0-0000-7000-8000-0000000000e1".into(), "claude-main".into())],
            custom_fields: vec!["industry".into()],
            secrets: vec!["SEARCH_API_KEY".into()],
            ..Default::default()
        };
        assert!(!workprocess(None).contains("This install"));
        let m = workprocess(Some(&names));
        assert!(m.contains("## 11. This install"));
        assert!(m.contains("Q4 SaaS founders (`0192f3a0-0000-7000-8000-0000000000c1`)"));
        assert!(m.contains("Warm leads"));
        assert!(m.contains("claude-main"));
        assert!(m.contains("`SEARCH_API_KEY`"));
        assert!(m.contains("secret('NAME')"));
    }

    /// Every fenced ```json block of the merged doc that is a whole workflow (has `format`).
    fn workflow_examples(doc: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let mut rest = doc;
        while let Some(start) = rest.find("```json\n") {
            let body = &rest[start + 8..];
            let Some(end) = body.find("\n```") else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&body[..end]) {
                if v.get("format").is_some() {
                    out.push(v);
                }
            }
            rest = &body[end + 4..];
        }
        out
    }

    #[test]
    fn every_json_example_in_the_doc_is_a_valid_workflow() {
        use crate::automations::validate::{Options, validate_value};
        let examples = workflow_examples(&workprocess(None));
        assert!(examples.len() >= 2, "the doc has the shape and the full example");
        for (i, ex) in examples.iter().enumerate() {
            let r = validate_value(ex, &Options::default());
            assert!(r.ok, "example {i} ({}): {:#?}", ex["name"], r.errors);
            assert!(r.warnings.iter().all(|w| w.code == "no_positions"), "example {i}: {:#?}", r.warnings);
        }
    }

    #[test]
    fn every_registry_node_is_in_the_merged_doc() {
        let m = workprocess(None);
        let reg = Registry::global();
        assert!(reg.type_ids().len() >= 59);
        for id in reg.type_ids() {
            assert!(m.contains(&format!("#### `{id}`")), "{id} is missing from workprocess.md");
        }
        // and the doc names no node type that does not exist
        let re = regex::Regex::new(r"`((?:trigger|aikido|ai|web|http|flow|data|code)\.[a-z_.]+)`").unwrap();
        for c in re.captures_iter(SOURCE) {
            let t = c[1].trim_end_matches('.');
            assert!(reg.get(t).is_some() || t.ends_with('.'), "workprocess.md names `{t}`, which is not a node type");
        }
    }

    #[test]
    fn the_doc_teaches_the_rules() {
        for part in ["Your answer", "Expressions", "Checklist", "Node catalog", "secret('NAME')", "{{first_name}}"] {
            assert!(SOURCE.contains(part), "{part}");
        }
    }

    #[test]
    fn type_labels() {
        use serde_json::json;
        assert_eq!(type_label(&json!({ "type": "string", "enum": ["a", "b"] }), 0), "a | b");
        assert_eq!(type_label(&json!({ "type": "array", "items": { "type": "string" } }), 0), "list of string");
        assert_eq!(type_label(&json!({ "type": "object", "properties": { "x": { "type": "integer" } }, "required": ["x"] }), 0), "{ x: integer }");
        assert_eq!(type_label(&json!({}), 0), "any");
    }
}
