//! A tiny builder for the JSON Schema of node params. Every node spec uses it, so the catalog,
//! the validator, `schema.json` and `workprocess.md` come from one place.
//!
//! The schemas it makes are *clean*: they say what a param is (`number`, `boolean`, a list...).
//! A `${{ }}` expression may stand for any value. The validator swaps each expression for a sample
//! value of the right type (`substitute_expressions`) before it checks the schema; the expression
//! itself is checked on its own and turned into the real type at run time.
use serde_json::{Map, Value, json};

pub fn string() -> Value {
    json!({ "type": "string" })
}

pub fn text(max_len: usize) -> Value {
    json!({ "type": "string", "maxLength": max_len })
}

pub fn integer(min: i64, max: i64) -> Value {
    json!({ "type": "integer", "minimum": min, "maximum": max })
}

pub fn number() -> Value {
    json!({ "type": "number" })
}

pub fn boolean() -> Value {
    json!({ "type": "boolean" })
}

pub fn one_of(values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values })
}

/// Any JSON value.
pub fn any() -> Value {
    json!({})
}

pub fn object() -> Value {
    json!({ "type": "object" })
}

pub fn string_map() -> Value {
    json!({ "type": "object", "additionalProperties": { "type": "string" } })
}

pub fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

pub fn string_list() -> Value {
    array(string())
}

/// Extra keys on a schema: description, default, UI hints.
pub trait SchemaExt {
    fn desc(self, text: &str) -> Value;
    fn default_value(self, v: Value) -> Value;
    /// `x-ui.widget`: `textarea`, `code`, `select`, `keyvalue`, `fields`, `rules`.
    fn widget(self, widget: &str) -> Value;
    /// `x-ui.source`: `campaigns`, `lists`, `sequences`, `accounts`, `providers`, `custom_fields`, `secrets`.
    fn source(self, source: &str) -> Value;
    fn max_items(self, n: usize) -> Value;
}

pub fn put(mut v: Value, key: &str, val: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_string(), val);
    }
    v
}

fn put_ui(mut v: Value, key: &str, val: &str) -> Value {
    if let Some(o) = v.as_object_mut() {
        let ui = o.entry("x-ui").or_insert_with(|| json!({}));
        if let Some(ui) = ui.as_object_mut() {
            ui.insert(key.to_string(), json!(val));
        }
    }
    v
}

impl SchemaExt for Value {
    fn desc(self, text: &str) -> Value {
        put(self, "description", json!(text))
    }
    fn default_value(self, v: Value) -> Value {
        put(self, "default", v)
    }
    fn widget(self, widget: &str) -> Value {
        put_ui(self, "widget", widget)
    }
    fn source(self, source: &str) -> Value {
        put_ui(self, "source", source)
    }
    fn max_items(self, n: usize) -> Value {
        put(self, "maxItems", json!(n))
    }
}

/// Builds an object schema.
#[derive(Default)]
pub struct Obj {
    props: Map<String, Value>,
    required: Vec<String>,
}

pub fn obj() -> Obj {
    Obj::default()
}

impl Obj {
    pub fn req(mut self, key: &str, schema: Value) -> Self {
        self.props.insert(key.to_string(), schema);
        self.required.push(key.to_string());
        self
    }
    pub fn opt(mut self, key: &str, schema: Value) -> Self {
        self.props.insert(key.to_string(), schema);
        self
    }
    pub fn build(self) -> Value {
        let mut o = Map::new();
        o.insert("type".into(), json!("object"));
        o.insert("properties".into(), Value::Object(self.props));
        if !self.required.is_empty() {
            o.insert("required".into(), json!(self.required));
        }
        Value::Object(o)
    }
}

/// Replace each expression string, where the schema wants another type, with a sample value of that
/// type. The result is checked against the clean schema, so an error keeps its exact place
/// (`params.output.fields[0].type`) and the expression itself is checked separately.
pub fn substitute_expressions(value: &Value, schema: &Value) -> Value {
    match value {
        Value::String(s) if has_expression(s) && wants_other_type(schema) => sample(schema),
        Value::Object(m) => {
            let props = schema.get("properties").and_then(Value::as_object);
            let extra = schema.get("additionalProperties").filter(|e| e.is_object());
            Value::Object(
                m.iter()
                    .map(|(k, v)| {
                        let sub = props.and_then(|p| p.get(k)).or(extra);
                        (k.clone(), sub.map_or_else(|| v.clone(), |sub| substitute_expressions(v, sub)))
                    })
                    .collect(),
            )
        }
        Value::Array(a) => match schema.get("items") {
            Some(items) => Value::Array(a.iter().map(|v| substitute_expressions(v, items)).collect()),
            None => value.clone(),
        },
        other => other.clone(),
    }
}

/// Does this schema want something other than free text?
fn wants_other_type(schema: &Value) -> bool {
    let ty = schema.get("type").and_then(Value::as_str);
    schema.get("enum").is_some() || matches!(ty, Some("integer" | "number" | "boolean" | "array" | "object"))
}

/// A value that satisfies the schema (used in place of an expression).
fn sample(schema: &Value) -> Value {
    if let Some(first) = schema.get("enum").and_then(Value::as_array).and_then(|e| e.first()) {
        return first.clone();
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") => json!(schema.get("minimum").and_then(Value::as_i64).unwrap_or(0)),
        Some("number") => json!(schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0)),
        Some("boolean") => json!(false),
        Some("array") => {
            let n = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0) as usize;
            Value::Array((0..n).map(|_| schema.get("items").map_or(Value::Null, sample)).collect())
        }
        Some("object") => {
            let props = schema.get("properties").and_then(Value::as_object);
            let required = schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
            Value::Object(required.filter_map(|k| props?.get(k).map(|p| (k.to_string(), sample(p)))).collect())
        }
        Some("string") => json!("x"),
        _ => Value::Null,
    }
}

/// Does this text hold an expression (`${{`, `${%` or `${#`)?
pub fn has_expression(s: &str) -> bool {
    s.contains("${{") || s.contains("${%") || s.contains("${#")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(schema: &Value, params: Value) -> Vec<String> {
        let v = jsonschema::validator_for(schema).unwrap();
        v.iter_errors(&substitute_expressions(&params, schema)).map(|e| format!("{}: {e}", e.instance_path())).collect()
    }

    #[test]
    fn a_non_string_param_accepts_an_expression() {
        let s = obj().req("n", integer(1, 10)).opt("flag", boolean()).opt("name", string()).opt("mode", one_of(&["a", "b"])).opt("tags", string_list()).build();
        assert!(check(&s, json!({ "n": 5 })).is_empty());
        assert!(check(&s, json!({ "n": "${{ json.count }}" })).is_empty());
        assert!(check(&s, json!({ "n": 1, "flag": "${{ vars.on }}", "mode": "${{ vars.m }}", "tags": "${{ json.tags }}" })).is_empty());
        assert!(!check(&s, json!({ "n": "five" })).is_empty(), "plain text is not a number");
        assert!(!check(&s, json!({ "n": 11 })).is_empty());
        assert!(!check(&s, json!({ "mode": "c" })).is_empty());
        assert!(!check(&s, json!({})).is_empty(), "n is required");
        // the clean schema alone stays strict
        let strict = jsonschema::validator_for(&s).unwrap();
        assert!(!strict.is_valid(&json!({ "n": "${{ json.count }}" })));
    }

    #[test]
    fn list_items_and_nested_objects_are_handled_and_keep_their_path() {
        let s = obj()
            .req("rules", array(obj().req("op", one_of(&["equals", "gt"])).opt("right", any()).build()))
            .opt("out", obj().opt("fields", array(obj().req("type", one_of(&["string", "number"])).build())).build())
            .build();
        assert!(check(&s, json!({ "rules": [{ "op": "equals", "right": 5 }] })).is_empty());
        assert!(check(&s, json!({ "rules": [{ "op": "${{ vars.op }}" }] })).is_empty());
        assert!(check(&s, json!({ "rules": "${{ nodes.x.json.rules }}" })).is_empty());
        let e = check(&s, json!({ "rules": [{ "op": "nope" }] }));
        assert!(e.iter().any(|m| m.starts_with("/rules/0/op")), "{e:?}");
        let e = check(&s, json!({ "rules": [], "out": { "fields": [{ "type": "weird" }] } }));
        assert!(e.iter().any(|m| m.starts_with("/out/fields/0/type")), "{e:?}");
    }

    #[test]
    fn a_sample_satisfies_required_fields_and_limits() {
        let s = obj().req("o", obj().req("a", integer(5, 9)).req("b", one_of(&["x", "y"])).build()).req("l", json!({ "type": "array", "minItems": 2, "items": integer(3, 4) })).build();
        assert!(check(&s, json!({ "o": "${{ json.o }}", "l": "${{ json.l }}" })).is_empty());
    }

    #[test]
    fn expression_text_is_found() {
        assert!(has_expression("a ${{ x }} b"));
        assert!(has_expression("${% if x %}y${% endif %}"));
        assert!(!has_expression("Hi {{first_name}}"));
    }
}
