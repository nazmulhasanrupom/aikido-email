//! Reads the style around a piece of text in an email (Type 1, Auto mode).
//!
//! It walks the tags around the text, from the outside in, and collects the font, size, color, bold, italic,
//! letter spacing and line height from: the tag itself (`<b>`, `<i>`, `<font>`), the rules of the `<style>`
//! blocks that match it, and its inline `style="..."`. The nearest tag wins, as in a mail app.
//!
//! Honest limit: it only sees what is written in the email HTML. A font the mail app adds, a web font, a
//! media query or a rule with `:hover` or `[attr]` are not read. A rule with `!important` beats an inline style.
use super::html::{Attr, Doc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_FONT: &str = "Arial";
pub const DEFAULT_SIZE: f32 = 14.0;
pub const DEFAULT_COLOR: &str = "#222222";

/// The style of the text image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub font: String,
    /// In CSS pixels.
    pub size: f32,
    /// `#rrggbb`
    pub color: String,
    pub bold: bool,
    pub italic: bool,
    pub letter_spacing: f32,
    /// In CSS pixels. `None`: the height of the font.
    pub line_height: Option<f32>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self { font: DEFAULT_FONT.into(), size: DEFAULT_SIZE, color: DEFAULT_COLOR.into(), bold: false, italic: false, letter_spacing: 0.0, line_height: None }
    }
}

/// What the user types in Manual mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ManualStyle {
    /// A font name such as `Arial`, `Georgia` or `Verdana`.
    pub font: String,
    /// In pixels, 6 to 72.
    pub size: f32,
    /// `#rrggbb`
    pub color: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

impl ManualStyle {
    pub fn validate(&self) -> Result<(), String> {
        if self.font.trim().is_empty() || self.font.len() > 100 {
            return Err("The font name is empty or too long.".into());
        }
        if !(6.0..=72.0).contains(&self.size) {
            return Err("The size must be between 6 and 72 pixels.".into());
        }
        if parse_color(&self.color).is_none() {
            return Err("The color must look like #222222.".into());
        }
        Ok(())
    }

    pub fn to_style(&self) -> TextStyle {
        let (r, g, b) = parse_color(&self.color).unwrap_or((0x22, 0x22, 0x22));
        TextStyle { font: self.font.trim().to_string(), size: self.size, color: format!("#{r:02x}{g:02x}{b:02x}"), bold: self.bold, italic: self.italic, letter_spacing: 0.0, line_height: None }
    }
}

// ---------- values ----------

const NAMED: [(&str, (u8, u8, u8)); 32] = [
    ("black", (0, 0, 0)),
    ("white", (255, 255, 255)),
    ("gray", (128, 128, 128)),
    ("grey", (128, 128, 128)),
    ("silver", (192, 192, 192)),
    ("red", (255, 0, 0)),
    ("green", (0, 128, 0)),
    ("blue", (0, 0, 255)),
    ("yellow", (255, 255, 0)),
    ("orange", (255, 165, 0)),
    ("purple", (128, 0, 128)),
    ("navy", (0, 0, 128)),
    ("teal", (0, 128, 128)),
    ("maroon", (128, 0, 0)),
    ("olive", (128, 128, 0)),
    ("lime", (0, 255, 0)),
    ("aqua", (0, 255, 255)),
    ("cyan", (0, 255, 255)),
    ("fuchsia", (255, 0, 255)),
    ("magenta", (255, 0, 255)),
    ("brown", (165, 42, 42)),
    ("pink", (255, 192, 203)),
    ("gold", (255, 215, 0)),
    ("indigo", (75, 0, 130)),
    ("darkgray", (169, 169, 169)),
    ("darkgrey", (169, 169, 169)),
    ("lightgray", (211, 211, 211)),
    ("lightgrey", (211, 211, 211)),
    ("dimgray", (105, 105, 105)),
    ("dimgrey", (105, 105, 105)),
    ("darkblue", (0, 0, 139)),
    ("darkred", (139, 0, 0)),
];

pub fn parse_color(v: &str) -> Option<(u8, u8, u8)> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return match hex.len() {
            3 | 4 => {
                let d = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|n| n * 17);
                Some((d(0)?, d(1)?, d(2)?))
            }
            6 | 8 => {
                let d = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
                Some((d(0)?, d(2)?, d(4)?))
            }
            _ => None,
        };
    }
    if let Some(args) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")).and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<f32> = args.split([',', ' ', '/']).filter(|p| !p.is_empty()).filter_map(|p| p.trim_end_matches('%').parse().ok()).collect();
        if parts.len() >= 3 {
            let c = |n: f32| n.clamp(0.0, 255.0).round() as u8;
            return Some((c(parts[0]), c(parts[1]), c(parts[2])));
        }
        return None;
    }
    NAMED.iter().find(|(n, _)| *n == v).map(|(_, c)| *c)
}

/// A length in pixels. `parent` is the size that `em` and `%` refer to.
pub fn parse_length(v: &str, parent: f32) -> Option<f32> {
    let v = v.trim().to_ascii_lowercase();
    let num = |suffix: &str| v.strip_suffix(suffix).and_then(|n| n.trim().parse::<f32>().ok());
    if let Some(n) = num("px") {
        return Some(n);
    }
    if let Some(n) = num("pt") {
        return Some(n * 4.0 / 3.0);
    }
    if let Some(n) = num("rem") {
        return Some(n * 16.0);
    }
    if let Some(n) = num("em") {
        return Some(n * parent);
    }
    if let Some(n) = num("%") {
        return Some(n / 100.0 * parent);
    }
    Some(match v.as_str() {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "0" => 0.0,
        _ => return None,
    })
}

/// The `size` of a `<font>` tag: 1 to 7, or `+1` and `-1` from 3.
fn font_tag_size(v: &str) -> Option<f32> {
    let v = v.trim();
    let n: i32 = if let Some(r) = v.strip_prefix('+') { 3 + r.parse::<i32>().ok()? } else if v.starts_with('-') { 3 + v.parse::<i32>().ok()? } else { v.parse().ok()? };
    Some(match n.clamp(1, 7) {
        1 => 10.0,
        2 => 13.0,
        3 => 16.0,
        4 => 18.0,
        5 => 24.0,
        6 => 32.0,
        _ => 48.0,
    })
}

// ---------- declarations and rules ----------

/// Split on `sep`, but not inside quotes or brackets.
fn split_outside(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut depth, mut quote, mut start) = (Vec::new(), 0i32, None::<char>, 0);
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, c) if c == sep && depth <= 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// `(property, value, important)` of a `style` attribute or the inside of a rule.
pub fn parse_declarations(s: &str) -> Vec<(String, String, bool)> {
    split_outside(s, ';')
        .into_iter()
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            let mut v = v.trim().to_string();
            let important = v.to_ascii_lowercase().ends_with("!important");
            if important {
                v.truncate(v.len() - "!important".len());
            }
            let k = k.trim().to_ascii_lowercase();
            (!k.is_empty() && !v.trim().is_empty()).then(|| (k, v.trim().to_string(), important))
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

#[derive(Debug, Clone)]
struct Rule {
    /// The compounds from left to right, joined by descendant links.
    sel: Vec<Compound>,
    spec: (u32, u32, u32),
    order: usize,
    decls: Vec<(String, String, bool)>,
}

fn parse_compound(s: &str) -> Option<Compound> {
    let mut c = Compound::default();
    let mut cur = String::new();
    let mut kind = 't';
    let flush = |c: &mut Compound, kind: char, cur: &mut String| {
        if !cur.is_empty() {
            match kind {
                '.' => c.classes.push(cur.to_ascii_lowercase()),
                '#' => c.id = Some(cur.to_ascii_lowercase()),
                _ if cur != "*" => c.tag = Some(cur.to_ascii_lowercase()),
                _ => {}
            }
            cur.clear();
        }
    };
    for ch in s.chars() {
        match ch {
            '.' | '#' => {
                flush(&mut c, kind, &mut cur);
                kind = ch;
            }
            ':' | '[' | '+' | '~' | '(' | ')' | '"' | '\'' | '=' => return None,
            _ => cur.push(ch),
        }
    }
    flush(&mut c, kind, &mut cur);
    Some(c)
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(a) = rest.find("/*") {
        out.push_str(&rest[..a]);
        rest = rest[a + 2..].find("*/").map_or("", |b| &rest[a + 2 + b + 2..]);
    }
    out.push_str(rest);
    out
}

fn parse_rules(blocks: &[&str]) -> Vec<Rule> {
    let mut rules = Vec::new();
    for block in blocks {
        let css = strip_comments(block);
        let mut rest = css.trim_start();
        while !rest.is_empty() {
            let Some(open) = rest.find('{') else { break };
            let head = rest[..open].trim();
            // find the matching close, counting nested braces (at-rules)
            let mut depth = 0;
            let mut close = rest.len();
            for (i, c) in rest[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let body = &rest[open + 1..close.min(rest.len())];
            rest = rest.get(close + 1..).unwrap_or("").trim_start();
            if head.starts_with('@') {
                continue; // @media, @font-face, @import ...
            }
            let decls = parse_declarations(body);
            for sel in head.split(',') {
                let parts: Vec<&str> = sel.split(|c: char| c.is_whitespace() || c == '>').filter(|p| !p.is_empty()).collect();
                let compounds: Option<Vec<Compound>> = parts.iter().map(|p| parse_compound(p)).collect();
                let Some(compounds) = compounds.filter(|c| !c.is_empty()) else { continue };
                let spec = compounds.iter().fold((0, 0, 0), |a, c| (a.0 + u32::from(c.id.is_some()), a.1 + c.classes.len() as u32, a.2 + u32::from(c.tag.is_some())));
                rules.push(Rule { sel: compounds, spec, order: rules.len(), decls: decls.clone() });
            }
        }
    }
    rules.sort_by_key(|r| (r.spec, r.order));
    rules
}

struct El {
    name: String,
    id: Option<String>,
    classes: Vec<String>,
    inline: Vec<(String, String, bool)>,
    attrs: Vec<Attr>,
}

fn el_of(name: &str, attrs: &[Attr]) -> El {
    let get = |n: &str| Doc::attr(attrs, n).and_then(|a| a.value.clone());
    El {
        name: name.to_string(),
        id: get("id").map(|v| v.trim().to_ascii_lowercase()),
        classes: get("class").map(|v| v.split_whitespace().map(str::to_ascii_lowercase).collect()).unwrap_or_default(),
        inline: get("style").map(|s| parse_declarations(&super::html::decode(&s))).unwrap_or_default(),
        attrs: attrs.to_vec(),
    }
}

fn compound_matches(c: &Compound, e: &El) -> bool {
    c.tag.as_ref().is_none_or(|t| *t == e.name) && c.id.as_ref().is_none_or(|i| e.id.as_ref() == Some(i)) && c.classes.iter().all(|k| e.classes.contains(k))
}

/// `chain[0]` is the element, the rest are its parents (nearest first).
fn rule_matches(r: &Rule, chain: &[&El]) -> bool {
    let Some((last, before)) = r.sel.split_last() else { return false };
    if !compound_matches(last, chain[0]) {
        return false;
    }
    let mut at = 1;
    for c in before.iter().rev() {
        match chain[at..].iter().position(|e| compound_matches(c, e)) {
            Some(p) => at += p + 1,
            None => return false,
        }
    }
    true
}

/// What the tag itself says (`<b>`, `<i>`, `<font color=...>`).
fn tag_declarations(e: &El) -> Vec<(String, String, bool)> {
    let d = |k: &str, v: &str| (k.to_string(), v.to_string(), false);
    let mut out = Vec::new();
    match e.name.as_str() {
        "b" | "strong" | "th" => out.push(d("font-weight", "bold")),
        "i" | "em" | "cite" | "dfn" => out.push(d("font-style", "italic")),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => out.push(d("font-weight", "bold")),
        "font" => {
            let a = super::html::attr_map(&e.attrs);
            if let Some(f) = a.get("face") {
                out.push(d("font-family", &super::html::decode(f)));
            }
            if let Some(s) = a.get("size").and_then(|s| font_tag_size(s)) {
                out.push(d("font-size", &format!("{s}px")));
            }
            if let Some(c) = a.get("color") {
                out.push(d("color", c));
            }
        }
        _ => {}
    }
    // `bgcolor`-style legacy attributes are not read
    out
}

/// The `font` shorthand: `italic bold 14px/20px Arial, sans-serif`.
fn expand_font_shorthand(v: &str) -> Vec<(String, String, bool)> {
    let toks = split_outside(v, ' ');
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let t = t.trim();
        let lower = t.to_ascii_lowercase();
        if lower == "italic" || lower == "oblique" {
            out.push(("font-style".to_string(), "italic".to_string(), false));
        } else if lower == "bold" || lower == "bolder" || matches!(lower.as_str(), "600" | "700" | "800" | "900") {
            out.push(("font-weight".to_string(), "bold".to_string(), false));
        } else if let Some(size_part) = lower.split('/').next().filter(|p| parse_length(p, 16.0).is_some()) {
            out.push(("font-size".to_string(), size_part.to_string(), false));
            if let Some((_, lh)) = lower.split_once('/') {
                out.push(("line-height".to_string(), lh.to_string(), false));
            }
            let family = toks[i + 1..].join(" ");
            if !family.trim().is_empty() {
                out.push(("font-family".to_string(), family.trim().to_string(), false));
            }
            break;
        }
    }
    out
}

// ---------- reading ----------

#[derive(Default)]
struct Props(HashMap<String, String>);

impl Props {
    fn set(&mut self, k: &str, v: &str) {
        if k == "font" {
            for (k, v, _) in expand_font_shorthand(v) {
                self.0.insert(k, v);
            }
        } else {
            self.0.insert(k.to_string(), v.to_string());
        }
    }
}

/// The style of the text node `text_tok`.
pub fn read_style(doc: &Doc<'_>, text_tok: usize) -> TextStyle {
    let rules = parse_rules(&doc.style_blocks());
    let chain_idx = doc.ancestors(text_tok); // nearest first
    let els: Vec<El> = chain_idx.iter().filter_map(|&i| doc.element(i)).map(|(n, a)| el_of(n, a)).collect();

    // the declared values of each element
    let mut declared: Vec<Props> = Vec::with_capacity(els.len());
    for k in 0..els.len() {
        let chain: Vec<&El> = els[k..].iter().collect();
        let mut p = Props::default();
        for (k, v, _) in tag_declarations(&els[k]) {
            p.set(&k, &v);
        }
        for r in rules.iter().filter(|r| rule_matches(r, &chain)) {
            for (k, v, imp) in &r.decls {
                if !imp {
                    p.set(k, v);
                }
            }
        }
        for (k, v, _) in &els[k].inline {
            p.set(k, v);
        }
        for r in rules.iter().filter(|r| rule_matches(r, &chain)) {
            for (k, v, imp) in &r.decls {
                if *imp {
                    p.set(k, v);
                }
            }
        }
        declared.push(p);
    }

    // inherit from the outside in
    let mut s = TextStyle::default();
    let (mut font, mut color_set) = (None::<String>, None::<String>);
    for p in declared.iter().rev() {
        if let Some(v) = p.0.get("font-family") {
            font = Some(v.clone());
        }
        if let Some(v) = p.0.get("font-size") {
            if let Some(px) = parse_length(v, s.size) {
                if px > 0.0 {
                    s.size = px;
                }
            }
        }
        if let Some(v) = p.0.get("color") {
            if let Some((r, g, b)) = parse_color(v) {
                color_set = Some(format!("#{r:02x}{g:02x}{b:02x}"));
            }
        }
        if let Some(v) = p.0.get("font-weight") {
            let w = v.trim().to_ascii_lowercase();
            s.bold = matches!(w.as_str(), "bold" | "bolder") || w.parse::<u32>().is_ok_and(|n| n >= 600);
        }
        if let Some(v) = p.0.get("font-style") {
            let w = v.trim().to_ascii_lowercase();
            s.italic = w == "italic" || w == "oblique";
        }
        if let Some(v) = p.0.get("letter-spacing") {
            s.letter_spacing = if v.trim().eq_ignore_ascii_case("normal") { 0.0 } else { parse_length(v, s.size).unwrap_or(s.letter_spacing) };
        }
        if let Some(v) = p.0.get("line-height") {
            let t = v.trim();
            s.line_height = if t.eq_ignore_ascii_case("normal") {
                None
            } else if let Ok(mult) = t.parse::<f32>() {
                Some(mult * s.size)
            } else {
                parse_length(t, s.size)
            };
        }
    }
    if let Some(f) = font {
        s.font = f;
    }
    if let Some(c) = color_set {
        s.color = c;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::html::{Doc, Tok};

    /// The style of the first text node that holds `word`.
    fn style_of(html: &str, word: &str) -> TextStyle {
        let doc = Doc::parse(html);
        let idx = doc.toks.iter().position(|t| matches!(t, Tok::Text{start,end} if html[*start..*end].contains(word))).unwrap();
        read_style(&doc, idx)
    }

    #[test]
    fn no_style_gives_the_safe_default() {
        let s = style_of("<p>Nazmul</p>", "Nazmul");
        assert_eq!(s, TextStyle::default());
        assert_eq!((s.font.as_str(), s.size, s.color.as_str()), ("Arial", 14.0, "#222222"));
    }

    #[test]
    fn inline_style_on_the_text_is_read() {
        let s = style_of(r#"<span style="font-size:18px; color:#336699; font-family:'Open Sans', Arial; letter-spacing:1px; line-height:24px">Nazmul</span>"#, "Nazmul");
        assert_eq!(s.size, 18.0);
        assert_eq!(s.color, "#336699");
        assert_eq!(s.font, "'Open Sans', Arial");
        assert_eq!(s.letter_spacing, 1.0);
        assert_eq!(s.line_height, Some(24.0));
    }

    #[test]
    fn bold_and_italic_come_from_the_tags_around() {
        let s = style_of("<p><b><i>Nazmul</i></b></p>", "Nazmul");
        assert!(s.bold && s.italic);
        let s = style_of(r#"<p><strong>Nazmul</strong></p>"#, "Nazmul");
        assert!(s.bold && !s.italic);
        let s = style_of(r#"<em>Nazmul</em>"#, "Nazmul");
        assert!(!s.bold && s.italic);
    }

    #[test]
    fn the_font_tag_is_read() {
        let s = style_of(r##"<font face="Verdana" size="2" color="#ff0000">Nazmul</font>"##, "Nazmul");
        assert_eq!((s.font.as_str(), s.size, s.color.as_str()), ("Verdana", 13.0, "#ff0000"));
    }

    #[test]
    fn a_parent_tag_sets_what_the_text_does_not() {
        let html = r#"<div style="font-family:Georgia,serif; color: rgb(10, 20, 30); font-size: 12pt"><p><span>Nazmul</span></p></div>"#;
        let s = style_of(html, "Nazmul");
        assert_eq!(s.font, "Georgia,serif");
        assert_eq!(s.color, "#0a141e");
        assert_eq!(s.size, 16.0);
    }

    #[test]
    fn the_nearest_tag_wins() {
        let s = style_of(r#"<div style="color:#111111;font-size:20px"><span style="color:#999999">Nazmul</span></div>"#, "Nazmul");
        assert_eq!(s.color, "#999999");
        assert_eq!(s.size, 20.0);
    }

    #[test]
    fn style_blocks_are_read_with_tag_class_and_id_rules() {
        let html = r#"<html><head><style>
            td { font-family: Verdana; color: #444 }
            .sig { font-size: 15px; font-weight: 700 }
            #name { font-style: italic }
            a:hover { color: red }
            @media (max-width: 600px) { .sig { font-size: 30px } }
        </style></head><body><table><tr><td><p class="sig"><span id="name">Nazmul</span></p></td></tr></table></body></html>"#;
        let s = style_of(html, "Nazmul");
        assert_eq!(s.font, "Verdana");
        assert_eq!(s.color, "#444444");
        assert_eq!(s.size, 15.0);
        assert!(s.bold && s.italic);
    }

    #[test]
    fn inline_beats_a_style_block_and_important_beats_inline() {
        let html = r#"<style>p { color: #111111 } .x { color: #222222 !important }</style><p style="color:#333333">Nazmul</p><p class="x" style="color:#444444">Hasan</p>"#;
        assert_eq!(style_of(html, "Nazmul").color, "#333333");
        assert_eq!(style_of(html, "Hasan").color, "#222222");
    }

    #[test]
    fn em_and_percent_follow_the_parent_size() {
        let s = style_of(r#"<div style="font-size:20px"><span style="font-size:80%">Nazmul</span></div>"#, "Nazmul");
        assert_eq!(s.size, 16.0);
        let s = style_of(r#"<div style="font-size:10px"><span style="font-size:1.5em">Nazmul</span></div>"#, "Nazmul");
        assert_eq!(s.size, 15.0);
    }

    #[test]
    fn the_font_shorthand_is_read() {
        let s = style_of(r#"<p style="font: italic bold 16px/22px Georgia, serif">Nazmul</p>"#, "Nazmul");
        assert!(s.bold && s.italic);
        assert_eq!(s.size, 16.0);
        assert_eq!(s.line_height, Some(22.0));
        assert_eq!(s.font, "Georgia, serif");
    }

    #[test]
    fn unitless_line_height_is_a_multiple_of_the_size() {
        let s = style_of(r#"<p style="font-size:10px; line-height:1.5">Nazmul</p>"#, "Nazmul");
        assert_eq!(s.line_height, Some(15.0));
    }

    #[test]
    fn colors_parse() {
        assert_eq!(parse_color("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_color("RED"), Some((255, 0, 0)));
        assert_eq!(parse_color("rgba(1, 2, 3, 0.5)"), Some((1, 2, 3)));
        assert_eq!(parse_color("nonsense"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn manual_style_is_checked() {
        let ok = ManualStyle { font: "Georgia".into(), size: 16.0, color: "#336699".into(), bold: true, italic: false };
        assert!(ok.validate().is_ok());
        assert_eq!(ok.to_style().color, "#336699");
        assert!(ManualStyle { size: 2.0, ..ok.clone() }.validate().is_err());
        assert!(ManualStyle { color: "blue-ish".into(), ..ok.clone() }.validate().is_err());
        assert!(ManualStyle { font: " ".into(), ..ok }.validate().is_err());
    }
}
