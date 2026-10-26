//! What a published version needs in `automation_triggers` (build map 9.1). Pure: no database.
//!
//! Every trigger node except `trigger.manual` becomes one row. The params of a trigger may use only
//! `${{ vars.* }}` (the validator checks it), so they are resolved here, once, when the workflow is switched on.
use super::{events, schedule};
use crate::automations::definition::{Node, WorkflowDefinition};
use crate::automations::expr::ExprEngine;
use rand::Rng;
use serde_json::{Map, Value, json};

/// One row of `automation_triggers`, before it is written.
#[derive(Debug, Clone)]
pub struct TriggerPlan {
    pub node_id: String,
    /// The node type, for example `trigger.lead_bounced`.
    pub type_id: String,
    /// `schedule`, `cron`, `webhook` or `event`.
    pub kind: &'static str,
    pub event_types: Vec<String>,
    /// The params with the variables filled in. Time triggers also get the resolved `timezone`.
    pub config: Value,
    pub webhook_path: Option<String>,
    /// The path was made up here (the node had none). A later publish keeps the old path.
    pub path_generated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanError {
    pub node_id: String,
    pub message: String,
}

fn err(node: &Node, message: impl Into<String>) -> PlanError {
    PlanError { node_id: node.id.clone(), message: message.into() }
}

/// 16 random letters and digits.
pub fn random_path() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..16).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

pub fn valid_path(p: &str) -> bool {
    (4..=64).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The params of a trigger node with `${{ vars.* }}` filled in.
pub fn resolve_params(def: &WorkflowDefinition, node: &Node) -> Result<Map<String, Value>, PlanError> {
    let vars: Map<String, Value> = def.variables.iter().map(|v| (v.key.clone(), v.value.clone().unwrap_or(Value::Null))).collect();
    let ctx = json!({ "vars": vars });
    let out = ExprEngine::new().eval_params(&Value::Object(node.params.clone()), &ctx).map_err(|e| err(node, format!("{}: {}", e.path, e.error)))?;
    Ok(out.as_object().cloned().unwrap_or_default())
}

/// The plan of one trigger node. `None` for `trigger.manual`.
pub fn plan_node(def: &WorkflowDefinition, node: &Node, default_tz: &str) -> Result<Option<TriggerPlan>, PlanError> {
    let mut p = resolve_params(def, node)?;
    // a variable that is empty means "not set": drop it, so the default applies
    p.retain(|_, v| !(v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty())));
    let tz = p.get("timezone").and_then(Value::as_str).map(str::to_string).or_else(|| def.settings.timezone.clone()).unwrap_or_else(|| default_tz.to_string());
    let type_id = node.type_id.as_str();
    let base = |kind: &'static str, config: Value| TriggerPlan { node_id: node.id.clone(), type_id: type_id.to_string(), kind, event_types: vec![], config, webhook_path: None, path_generated: false };
    match type_id {
        "trigger.manual" => Ok(None),
        "trigger.schedule" | "trigger.cron" => {
            schedule::parse_tz(&tz).map_err(|m| err(node, m))?;
            p.insert("timezone".into(), json!(tz));
            let kind = if type_id == "trigger.cron" { "cron" } else { "schedule" };
            let config = Value::Object(p);
            // find out now whether it can ever fire
            schedule::first_fire(kind, &config, chrono::Utc::now()).map_err(|m| err(node, m))?;
            Ok(Some(base(kind, config)))
        }
        "trigger.webhook" => {
            let (path, generated) = match p.get("path").and_then(Value::as_str) {
                Some(path) if valid_path(path) => (path.to_string(), false),
                Some(path) => return Err(err(node, format!("The path `{path}` must be 4 to 64 letters, digits, `_` or `-`."))),
                None => (random_path(), true),
            };
            let method = p.get("method").and_then(Value::as_str).unwrap_or("POST").to_ascii_uppercase();
            if !["POST", "GET", "PUT", "ANY"].contains(&method.as_str()) {
                return Err(err(node, format!("The method `{method}` is not one of POST, GET, PUT, ANY.")));
            }
            let auth = p.get("auth").and_then(Value::as_str).unwrap_or("none").to_string();
            let response = p.get("response").and_then(Value::as_str).unwrap_or("immediately").to_string();
            let mut config = json!({ "path": path, "method": method, "auth": auth, "response": response });
            if auth == "header" {
                let header = p.get("header_name").and_then(Value::as_str);
                let secret = p.get("secret").and_then(Value::as_str);
                match (header, secret) {
                    (Some(h), Some(s)) if axum::http::HeaderName::from_bytes(h.as_bytes()).is_ok() && crate::automations::secrets::valid_name(s) => {
                        config["header_name"] = json!(h.to_ascii_lowercase());
                        config["secret"] = json!(s);
                    }
                    _ => return Err(err(node, "Auth `header` needs `header_name` (a header name) and `secret` (the NAME of a secret).")),
                }
            } else if auth != "none" {
                return Err(err(node, format!("The auth `{auth}` is not `none` or `header`.")));
            }
            Ok(Some(TriggerPlan { webhook_path: Some(path), path_generated: generated, ..base("webhook", config) }))
        }
        _ if events::source_of(type_id).is_some() => {
            let mut plan = base("event", Value::Object(p));
            plan.event_types = events::event_types(type_id);
            Ok(Some(plan))
        }
        other => Err(err(node, format!("`{other}` is not a trigger type that can run in production."))),
    }
}

/// The plans of all trigger nodes of a version.
pub fn plan_triggers(def: &WorkflowDefinition, default_tz: &str) -> Result<Vec<TriggerPlan>, PlanError> {
    let reg = crate::automations::registry::Registry::global();
    let mut out = Vec::new();
    for node in def.nodes.iter().filter(|n| reg.spec(&n.type_id).is_some_and(|s| s.is_trigger())) {
        if node.disabled {
            continue;
        }
        if let Some(p) = plan_node(def, node, default_tz)? {
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(nodes: Value, variables: Value) -> WorkflowDefinition {
        serde_json::from_value(json!({ "format": "aikido-workflow", "version": 1, "name": "T", "variables": variables, "nodes": nodes, "edges": [] })).unwrap()
    }

    #[test]
    fn a_manual_trigger_makes_no_row() {
        let d = def(json!([{ "id": "start", "type": "trigger.manual" }]), json!([]));
        assert!(plan_triggers(&d, "UTC").unwrap().is_empty());
    }

    #[test]
    fn variables_are_filled_in_and_an_empty_one_means_any() {
        let c = "0192f3a0-0000-7000-8000-000000000002";
        let d = def(
            json!([{ "id": "a", "type": "trigger.lead_added_to_campaign", "params": { "campaign_id": "${{ vars.campaign_id }}" } },
                   { "id": "b", "type": "trigger.reply_received", "params": { "campaign_id": "${{ vars.nothing }}" } }]),
            json!([{ "key": "campaign_id", "kind": "campaign", "value": c }, { "key": "nothing", "kind": "text", "value": null }]),
        );
        let plans = plan_triggers(&d, "UTC").unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].kind, "event");
        assert_eq!(plans[0].config["campaign_id"], c);
        assert_eq!(plans[0].event_types, vec!["campaign_added"]);
        assert!(plans[1].config.get("campaign_id").is_none(), "{}", plans[1].config);
        assert_eq!(plans[1].event_types, vec!["inbound_message"]);
    }

    #[test]
    fn a_time_trigger_gets_its_time_zone() {
        let d = def(
            json!([{ "id": "tick", "type": "trigger.cron", "params": { "expression": "0 9 * * 1-5" } },
                   { "id": "tock", "type": "trigger.schedule", "params": { "mode": "interval", "every": 30, "unit": "minutes", "timezone": "Europe/Berlin" } }]),
            json!([]),
        );
        let plans = plan_triggers(&d, "Asia/Dhaka").unwrap();
        assert_eq!((plans[0].kind, plans[0].config["timezone"].as_str()), ("cron", Some("Asia/Dhaka")));
        assert_eq!((plans[1].kind, plans[1].config["timezone"].as_str()), ("schedule", Some("Europe/Berlin")));
        // the workflow time zone beats the install default
        let mut d2 = d.clone();
        d2.settings.timezone = Some("UTC".into());
        assert_eq!(plan_triggers(&d2, "Asia/Dhaka").unwrap()[0].config["timezone"], "UTC");
    }

    #[test]
    fn a_bad_time_trigger_is_refused_with_its_node() {
        let d = def(json!([{ "id": "tick", "type": "trigger.cron", "params": { "expression": "0 0 9 * * *" } }]), json!([]));
        let e = plan_triggers(&d, "UTC").unwrap_err();
        assert_eq!(e.node_id, "tick");
        assert!(e.message.contains("5 fields"), "{}", e.message);
        let d = def(json!([{ "id": "tick", "type": "trigger.cron", "params": { "expression": "0 9 * * *", "timezone": "Mars/Base" } }]), json!([]));
        assert!(plan_triggers(&d, "UTC").unwrap_err().message.contains("time zone"));
        let d = def(json!([{ "id": "s", "type": "trigger.schedule", "params": { "mode": "interval", "every": 1, "unit": "weeks" } }]), json!([]));
        assert!(plan_triggers(&d, "UTC").is_err());
    }

    #[test]
    fn a_webhook_gets_a_path_a_method_and_an_auth_rule() {
        let d = def(json!([{ "id": "hook", "type": "trigger.webhook", "params": { "path": "my-form_1", "method": "post", "auth": "header", "header_name": "X-Api-Secret", "secret": "FORM_SECRET" } }]), json!([]));
        let p = &plan_triggers(&d, "UTC").unwrap()[0];
        assert_eq!((p.kind, p.webhook_path.as_deref(), p.path_generated), ("webhook", Some("my-form_1"), false));
        assert_eq!(p.config["method"], "POST");
        assert_eq!(p.config["header_name"], "x-api-secret");
        assert_eq!(p.config["secret"], "FORM_SECRET");
        // no path: made up, 16 characters
        let d = def(json!([{ "id": "hook", "type": "trigger.webhook" }]), json!([]));
        let p = &plan_triggers(&d, "UTC").unwrap()[0];
        assert!(p.path_generated && valid_path(p.webhook_path.as_deref().unwrap()) && p.webhook_path.as_deref().unwrap().len() == 16);
        assert_eq!(p.config["auth"], "none");
        assert_eq!(p.config["response"], "immediately");
    }

    #[test]
    fn a_bad_webhook_is_refused() {
        let bad = |params: Value| plan_triggers(&def(json!([{ "id": "hook", "type": "trigger.webhook", "params": params }]), json!([])), "UTC").unwrap_err();
        assert!(bad(json!({ "path": "a/b" })).message.contains("path"));
        assert!(bad(json!({ "path": "abc" })).message.contains("path"));
        assert!(bad(json!({ "auth": "header" })).message.contains("header_name"));
        assert!(bad(json!({ "auth": "header", "header_name": "x-key", "secret": "lower" })).message.contains("secret"));
        assert!(bad(json!({ "method": "DELETE" })).message.contains("method"));
    }

    #[test]
    fn a_variable_without_a_value_is_an_error_that_names_the_node() {
        let d = def(json!([{ "id": "a", "type": "trigger.lead_added_to_campaign", "params": { "campaign_id": "${{ vars.missing }}" } }]), json!([]));
        let e = plan_triggers(&d, "UTC").unwrap_err();
        assert_eq!(e.node_id, "a");
        assert!(e.message.contains("params.campaign_id"), "{}", e.message);
    }

    #[test]
    fn a_disabled_trigger_is_left_out() {
        let d = def(json!([{ "id": "a", "type": "trigger.lead_created", "disabled": true }, { "id": "b", "type": "trigger.lead_created" }]), json!([]));
        let plans = plan_triggers(&d, "UTC").unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].node_id, "b");
    }

    #[test]
    fn paths() {
        assert!(valid_path("abcd") && valid_path("A_b-9"));
        assert!(!valid_path("abc") && !valid_path("a b cd") && !valid_path(&"a".repeat(65)));
        assert_ne!(random_path(), random_path());
    }
}
