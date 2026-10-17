//! Templates for the subject and body of a step. Pure: no DB, no clock, no network.
//!
//! Syntax: `{{name}}` and `{{name|fallback}}`. Spaces inside the braces are ignored.
//! A value that is missing or empty uses the fallback. No fallback means the render fails.
//!
//! An AI slot is `{{ai:key}}` or `{{ai:key|fallback}}`. Its text comes from the Email Generator, and it is given to
//! `render` as the value `ai:key` (see `ai_var`). The name rules below do not allow a `:` in a normal variable, so a
//! caller value can never take the place of an AI text. `variables`, `required` and `used` skip AI slots.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text(String),
    Var { name: String, fallback: Option<String> },
    /// `{{ai:key}}`
    Ai { key: String, fallback: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TemplateError {
    pub message: String,
}

/// Where the text goes. Decides how a value is cleaned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// Plain text body.
    Text,
    /// HTML body: `& < > " '` in values become entities.
    Html,
    /// Subject: a line break in a value becomes a space.
    Subject,
}

/// Names that every enrollment has. A caller value with the same name does not replace them.
pub const BUILTINS: [&str; 3] = ["email", "sender_name", "sender_email"];

pub type Vars = BTreeMap<String, String>;

/// The name under which `render` looks for the text of an AI slot.
pub fn ai_var(key: &str) -> String {
    format!("ai:{key}")
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Template {
    pub fn parse(src: &str) -> Result<Template, TemplateError> {
        let mut parts = Vec::new();
        let mut rest = src;
        while let Some(open) = rest.find("{{") {
            if open > 0 {
                parts.push(Part::Text(rest[..open].to_string()));
            }
            let after = &rest[open + 2..];
            let Some(close) = after.find("}}") else {
                return Err(TemplateError { message: "A '{{' has no closing '}}'.".into() });
            };
            let inner = &after[..close];
            let (name, fallback) = match inner.split_once('|') {
                Some((n, f)) => (n.trim(), Some(f.trim().to_string())),
                None => (inner.trim(), None),
            };
            if let Some(key) = name.strip_prefix("ai:") {
                let key = key.trim();
                if !valid_name(key) {
                    return Err(TemplateError {
                        message: format!("'{{{{{inner}}}}}' is not valid. An AI slot is written {{{{ai:key}}}}, and the key has letters, digits and '_', and starts with a letter."),
                    });
                }
                parts.push(Part::Ai { key: key.to_string(), fallback });
                rest = &after[close + 2..];
                continue;
            }
            if !valid_name(name) {
                return Err(TemplateError {
                    message: format!("'{{{{{inner}}}}}' is not valid. A variable name has letters, digits and '_', and starts with a letter."),
                });
            }
            parts.push(Part::Var { name: name.to_string(), fallback });
            rest = &after[close + 2..];
        }
        if !rest.is_empty() {
            parts.push(Part::Text(rest.to_string()));
        }
        Ok(Template { parts })
    }

    /// Every variable the template uses: (name, has a fallback). A name can appear twice.
    pub fn variables(&self) -> Vec<(&str, bool)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Var { name, fallback } => Some((name.as_str(), fallback.is_some())),
                Part::Text(_) | Part::Ai { .. } => None,
            })
            .collect()
    }

    /// Every AI slot the template uses: (key, has an inline fallback). A key can appear twice.
    pub fn ai_keys(&self) -> Vec<(&str, bool)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Ai { key, fallback } => Some((key.as_str(), fallback.is_some())),
                _ => None,
            })
            .collect()
    }

    /// Names that have a value in no case: used without a fallback.
    pub fn required(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.variables().into_iter().filter(|(_, fb)| !fb).map(|(n, _)| n).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Fills the template. `Err` lists the names that have no value and no fallback.
    pub fn render(&self, vars: &Vars, escape: Escape) -> Result<String, Vec<String>> {
        let mut out = String::new();
        let mut missing: Vec<String> = Vec::new();
        for part in &self.parts {
            match part {
                Part::Text(t) => out.push_str(t),
                Part::Ai { key, fallback } => {
                    let name = ai_var(key);
                    match vars.get(&name).filter(|v| !v.trim().is_empty()) {
                        Some(v) => out.push_str(&clean(v, escape)),
                        None => match fallback {
                            Some(f) => out.push_str(f),
                            None => {
                                if !missing.contains(&name) {
                                    missing.push(name);
                                }
                            }
                        },
                    }
                }
                Part::Var { name, fallback } => match vars.get(name).filter(|v| !v.trim().is_empty()) {
                    Some(v) => out.push_str(&clean(v, escape)),
                    None => match fallback {
                        Some(f) => out.push_str(f),
                        None => {
                            if !missing.contains(name) {
                                missing.push(name.clone());
                            }
                        }
                    },
                },
            }
        }
        if missing.is_empty() { Ok(out) } else { Err(missing) }
    }
}

fn clean(value: &str, escape: Escape) -> String {
    match escape {
        Escape::Text => value.to_string(),
        Escape::Subject => value.replace(['\r', '\n'], " "),
        Escape::Html => {
            let mut s = String::with_capacity(value.len());
            for c in value.chars() {
                match c {
                    '&' => s.push_str("&amp;"),
                    '<' => s.push_str("&lt;"),
                    '>' => s.push_str("&gt;"),
                    '"' => s.push_str("&quot;"),
                    '\'' => s.push_str("&#39;"),
                    c => s.push(c),
                }
            }
            s
        }
    }
}

/// The values of one enrollment, as plain text, plus the built-in names.
/// The caller's `variables` must be a JSON object of text, number or true/false values.
pub fn build_vars(variables: &Value, email: &str, sender_name: &str, sender_email: &str) -> Result<Vars, String> {
    let mut vars = Vars::new();
    match variables {
        Value::Null => {}
        Value::Object(map) => {
            // 50 from the caller, plus the fields of the lead (up to 9 text fields and 50 custom fields)
            if map.len() > 150 {
                return Err("variables can have at most 150 entries.".into());
            }
            for (k, v) in map {
                if !valid_name(k) {
                    return Err(format!("'{k}' is not a valid variable name."));
                }
                let text = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => continue,
                    _ => return Err(format!("The value of '{k}' must be text, a number, or true/false.")),
                };
                if text.chars().count() > 2000 {
                    return Err(format!("The value of '{k}' is longer than 2000 characters."));
                }
                vars.insert(k.clone(), text);
            }
        }
        _ => return Err("variables must be a JSON object.".into()),
    }
    vars.insert("email".into(), email.to_string());
    vars.insert("sender_name".into(), sender_name.to_string());
    vars.insert("sender_email".into(), sender_email.to_string());
    Ok(vars)
}

/// What to show as the sender name when the account has no display name: the part before the `@`.
pub fn sender_name_of(display_name: &str, email: &str) -> String {
    if display_name.trim().is_empty() { email.split('@').next().unwrap_or(email).to_string() } else { display_name.trim().to_string() }
}

/// One step as text, before the variables are filled.
#[derive(Clone, Debug)]
pub struct StepTemplates {
    pub subject: Template,
    pub body_text: Option<Template>,
    pub body_html: Option<Template>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

impl StepTemplates {
    /// Parse the three texts of a step. An empty text is `None` (for the subject: an empty template).
    pub fn parse(subject: &str, body_text: Option<&str>, body_html: Option<&str>) -> Result<Self, TemplateError> {
        let prefix = |what: &str, e: TemplateError| TemplateError { message: format!("{what}: {}", e.message) };
        fn body(t: Option<&str>) -> Option<&str> {
            t.filter(|t| !t.trim().is_empty())
        }
        Ok(Self {
            subject: Template::parse(subject).map_err(|e| prefix("subject", e))?,
            body_text: body(body_text).map(Template::parse).transpose().map_err(|e| prefix("body_text", e))?,
            body_html: body(body_html).map(Template::parse).transpose().map_err(|e| prefix("body_html", e))?,
        })
    }

    /// Names with no fallback, over all three texts.
    pub fn required(&self) -> Vec<String> {
        let mut names: Vec<String> = self.subject.required().into_iter().map(String::from).collect();
        for t in [&self.body_text, &self.body_html].into_iter().flatten() {
            names.extend(t.required().into_iter().map(String::from));
        }
        names.sort();
        names.dedup();
        names
    }

    /// Every name the step uses, with or without a fallback, over all three texts.
    pub fn used(&self) -> Vec<String> {
        let mut names: Vec<String> = self.subject.variables().into_iter().map(|(n, _)| n.to_string()).collect();
        for t in [&self.body_text, &self.body_html].into_iter().flatten() {
            names.extend(t.variables().into_iter().map(|(n, _)| n.to_string()));
        }
        names.sort();
        names.dedup();
        names
    }

    /// Every AI slot key the step uses, over all three texts, without doubles.
    pub fn ai_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.subject.ai_keys().into_iter().map(|(k, _)| k.to_string()).collect();
        for t in [&self.body_text, &self.body_html].into_iter().flatten() {
            keys.extend(t.ai_keys().into_iter().map(|(k, _)| k.to_string()));
        }
        keys.sort();
        keys.dedup();
        keys
    }

    /// The AI slot keys that have an inline fallback `{{ai:key|fallback}}` in every place they are used.
    pub fn ai_keys_with_inline_fallback(&self) -> Vec<String> {
        let mut all: Vec<(String, bool)> = Vec::new();
        for t in std::iter::once(&self.subject).chain([&self.body_text, &self.body_html].into_iter().flatten()) {
            all.extend(t.ai_keys().into_iter().map(|(k, f)| (k.to_string(), f)));
        }
        let mut keys: Vec<String> = all.iter().filter(|(k, _)| all.iter().filter(|(k2, _)| k2 == k).all(|(_, f)| *f)).map(|(k, _)| k.clone()).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// The AI slot keys used in the subject.
    pub fn ai_keys_in_subject(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.subject.ai_keys().into_iter().map(|(k, _)| k.to_string()).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Fill all three texts. `Err` lists every missing name once (an AI slot is listed as `ai:key`).
    pub fn render(&self, vars: &Vars) -> Result<Rendered, Vec<String>> {
        let mut missing: Vec<String> = Vec::new();
        let mut take = |r: Result<String, Vec<String>>| match r {
            Ok(s) => Some(s),
            Err(m) => {
                for n in m {
                    if !missing.contains(&n) {
                        missing.push(n);
                    }
                }
                None
            }
        };
        let subject = take(self.subject.render(vars, Escape::Subject));
        let body_text = self.body_text.as_ref().map(|t| take(t.render(vars, Escape::Text)));
        let body_html = self.body_html.as_ref().map(|t| take(t.render(vars, Escape::Html)));
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(Rendered { subject: subject.unwrap_or_default(), body_text: body_text.flatten(), body_html: body_html.flatten() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, &str)]) -> Vars {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn render(src: &str, v: &Vars) -> Result<String, Vec<String>> {
        Template::parse(src).unwrap().render(v, Escape::Text)
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(render("Hello there.", &vars(&[])).unwrap(), "Hello there.");
        assert_eq!(render("", &vars(&[])).unwrap(), "");
        // A single brace is not a variable.
        assert_eq!(render("a { b } c }} d", &vars(&[])).unwrap(), "a { b } c }} d");
    }

    #[test]
    fn a_value_is_filled_in() {
        assert_eq!(render("Hi {{first_name}}!", &vars(&[("first_name", "Ana")])).unwrap(), "Hi Ana!");
        assert_eq!(render("{{a}}{{b}}", &vars(&[("a", "1"), ("b", "2")])).unwrap(), "12");
        assert_eq!(render("Hi {{ first_name }}!", &vars(&[("first_name", "Ana")])).unwrap(), "Hi Ana!");
    }

    #[test]
    fn a_fallback_is_used_when_the_value_is_missing_or_empty() {
        assert_eq!(render("Hi {{first_name|there}}!", &vars(&[])).unwrap(), "Hi there!");
        assert_eq!(render("Hi {{first_name|there}}!", &vars(&[("first_name", "")])).unwrap(), "Hi there!");
        assert_eq!(render("Hi {{first_name|there}}!", &vars(&[("first_name", "   ")])).unwrap(), "Hi there!");
        assert_eq!(render("Hi {{ first_name | there }}!", &vars(&[])).unwrap(), "Hi there!");
        // The fallback can be empty on purpose.
        assert_eq!(render("Hi{{ title| }}!", &vars(&[])).unwrap(), "Hi!");
        // A value beats the fallback.
        assert_eq!(render("Hi {{first_name|there}}!", &vars(&[("first_name", "Ana")])).unwrap(), "Hi Ana!");
    }

    #[test]
    fn a_missing_value_without_a_fallback_fails_and_lists_each_name_once() {
        assert_eq!(render("{{a}} {{b}} {{a}}", &vars(&[])).unwrap_err(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(render("{{a}} {{b}}", &vars(&[("a", "1")])).unwrap_err(), vec!["b".to_string()]);
    }

    #[test]
    fn bad_syntax_is_refused() {
        assert!(Template::parse("Hi {{name").unwrap_err().message.contains("closing"));
        assert!(Template::parse("Hi {{}}").is_err());
        assert!(Template::parse("Hi {{ |x}}").is_err());
        assert!(Template::parse("Hi {{1abc}}").is_err());
        assert!(Template::parse("Hi {{a b}}").is_err());
        assert!(Template::parse("Hi {{a-b}}").is_err());
    }

    #[test]
    fn html_values_are_escaped_but_the_template_is_not() {
        let t = Template::parse("<p>Hi {{name}}</p>").unwrap();
        let out = t.render(&vars(&[("name", "<script>alert(\"x\")</script> & 'co'")]), Escape::Html).unwrap();
        assert_eq!(out, "<p>Hi &lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;co&#39;</p>");
    }

    #[test]
    fn a_line_break_in_a_subject_value_becomes_a_space() {
        let t = Template::parse("Hi {{name}}").unwrap();
        let out = t.render(&vars(&[("name", "Ana\r\nBcc: evil@example.com")]), Escape::Subject).unwrap();
        assert!(!out.contains(['\r', '\n']));
        assert_eq!(out, "Hi Ana  Bcc: evil@example.com");
    }

    #[test]
    fn required_names_skip_the_ones_with_a_fallback() {
        let t = Template::parse("{{a}} {{b|x}} {{c}} {{a}}").unwrap();
        assert_eq!(t.required(), vec!["a", "c"]);
        assert_eq!(t.variables().len(), 4);
    }

    #[test]
    fn built_ins_win_over_caller_values() {
        let v = build_vars(&json!({ "first_name": "Ana", "email": "evil@x.com", "n": 3, "ok": true, "skip": null }), "ana@co.com", "Sam", "sam@me.com")
            .unwrap();
        assert_eq!(v["email"], "ana@co.com");
        assert_eq!(v["sender_name"], "Sam");
        assert_eq!(v["sender_email"], "sam@me.com");
        assert_eq!(v["first_name"], "Ana");
        assert_eq!(v["n"], "3");
        assert_eq!(v["ok"], "true");
        assert!(!v.contains_key("skip"));
    }

    #[test]
    fn bad_variables_are_refused() {
        let e = |v: Value| build_vars(&v, "a@b.com", "S", "s@b.com").unwrap_err();
        assert!(e(json!([1])).contains("JSON object"));
        assert!(e(json!({ "a": [1] })).contains("text, a number"));
        assert!(e(json!({ "a": { "b": 1 } })).contains("text, a number"));
        assert!(e(json!({ "a b": "x" })).contains("not a valid variable name"));
        assert!(e(json!({ "a": "x".repeat(2001) })).contains("2000"));
        let many: serde_json::Map<String, Value> = (0..151).map(|i| (format!("k{i}"), json!("v"))).collect();
        assert!(e(Value::Object(many)).contains("150"));
        assert!(build_vars(&Value::Null, "a@b.com", "S", "s@b.com").is_ok());
    }

    #[test]
    fn sender_name_falls_back_to_the_address_start() {
        assert_eq!(sender_name_of("Sam Lee", "sam@x.com"), "Sam Lee");
        assert_eq!(sender_name_of("  ", "sam@x.com"), "sam");
    }

    #[test]
    fn a_step_reports_every_missing_name() {
        let st = StepTemplates::parse("Hi {{first_name}}", Some("About {{company}} and {{first_name}}"), Some("<p>{{role}}</p>")).unwrap();
        assert_eq!(st.required(), vec!["company", "first_name", "role"]);
        let missing = st.render(&vars(&[("company", "Acme")])).unwrap_err();
        assert_eq!(missing, vec!["first_name".to_string(), "role".to_string()]);
        let ok = st.render(&vars(&[("company", "Acme"), ("first_name", "Ana"), ("role", "CTO")])).unwrap();
        assert_eq!(ok.subject, "Hi Ana");
        assert_eq!(ok.body_text.as_deref(), Some("About Acme and Ana"));
        assert_eq!(ok.body_html.as_deref(), Some("<p>CTO</p>"));
    }

    #[test]
    fn ai_slots_parse_render_and_stay_out_of_the_normal_lists() {
        let t = Template::parse("Hi {{first_name|there}}, {{ai:opener}} and {{ ai:tail | bye }}.").unwrap();
        assert_eq!(t.ai_keys(), vec![("opener", false), ("tail", true)]);
        assert_eq!(t.variables(), vec![("first_name", true)]);
        assert!(t.required().is_empty());
        // the text is filled in; a missing one uses the inline fallback; no fallback is reported as `ai:key`
        let v = vars(&[("ai:opener", "Nice site"), ("ai:tail", "Cheers")]);
        assert_eq!(t.render(&v, Escape::Text).unwrap(), "Hi there, Nice site and Cheers.");
        assert_eq!(t.render(&vars(&[("ai:opener", "x")]), Escape::Text).unwrap(), "Hi there, x and bye.");
        assert_eq!(t.render(&vars(&[]), Escape::Text).unwrap_err(), vec!["ai:opener".to_string()]);
        // an empty AI text counts as missing
        assert_eq!(t.render(&vars(&[("ai:opener", "  ")]), Escape::Text).unwrap_err(), vec!["ai:opener".to_string()]);
    }

    #[test]
    fn ai_text_is_escaped_like_any_value() {
        let t = Template::parse("<p>{{ai:a}}</p>").unwrap();
        assert_eq!(t.render(&vars(&[("ai:a", "<b>&")]), Escape::Html).unwrap(), "<p>&lt;b&gt;&amp;</p>");
        let s = Template::parse("{{ai:a}}").unwrap();
        assert_eq!(s.render(&vars(&[("ai:a", "x\r\ny")]), Escape::Subject).unwrap(), "x  y");
    }

    #[test]
    fn a_bad_ai_slot_is_refused_and_a_normal_variable_cannot_hold_a_colon() {
        assert!(Template::parse("{{ai:}}").is_err());
        assert!(Template::parse("{{ai:1x}}").is_err());
        assert!(Template::parse("{{ai:a b}}").is_err());
        assert!(Template::parse("{{ai}}").is_ok()); // a normal variable that happens to be called `ai`
        assert!(Template::parse("{{x:y}}").is_err());
        // a caller value can not take the place of an AI text
        assert!(build_vars(&json!({ "ai:opener": "evil" }), "a@b.com", "S", "s@b.com").is_err());
    }

    #[test]
    fn a_step_lists_its_ai_keys() {
        let st = StepTemplates::parse("Hi {{ai:subj|x}}", Some("{{ai:a}} {{ai:b|fb}} {{ai:b|fb}} {{ai:c|fb}} {{ai:c}}"), Some("<p>{{ai:a}}</p>")).unwrap();
        assert_eq!(st.ai_keys(), vec!["a", "b", "c", "subj"]);
        assert_eq!(st.ai_keys_with_inline_fallback(), vec!["b", "subj"]);
        assert_eq!(st.ai_keys_in_subject(), vec!["subj"]);
        assert!(st.required().is_empty());
        assert!(st.used().is_empty());
    }

    #[test]
    fn step_parse_names_the_bad_field() {
        assert!(StepTemplates::parse("ok", Some("{{x"), None).unwrap_err().message.starts_with("body_text:"));
        assert!(StepTemplates::parse("{{x", None, None).unwrap_err().message.starts_with("subject:"));
        assert!(StepTemplates::parse("ok", None, Some("{{")).unwrap_err().message.starts_with("body_html:"));
    }
}
