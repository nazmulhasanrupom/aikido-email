//! A small HTML reader that keeps the exact bytes of the email. It splits the HTML into tokens with their byte
//! ranges, so a tracker can be put in with a plain string edit and nothing else in the email changes.
//!
//! It is not a full HTML parser. It knows tags, attributes, comments, the raw text of `<style>` and `<script>`,
//! and void tags. An end tag closes the nearest open tag with the same name. That is enough to find text, find
//! an `<img>` outside a link, and walk up the tags around a piece of text.
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Attr {
    /// Lower case.
    pub name: String,
    /// The raw value (not decoded), without quotes.
    pub value: Option<String>,
    /// Byte range of the raw value inside the HTML (without quotes).
    pub range: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub enum Tok {
    Text { start: usize, end: usize },
    Open { name: String, attrs: Vec<Attr>, start: usize, end: usize, self_closing: bool },
    Close { name: String, start: usize, end: usize },
    /// The content of `<style>`, `<script>`, `<title>` or `<textarea>`.
    Raw { name: String, start: usize, end: usize },
    /// A comment, a doctype or a processing instruction.
    Other,
}

const VOID: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const RAW: [&str; 4] = ["script", "style", "title", "textarea"];

pub fn is_void(name: &str) -> bool {
    VOID.contains(&name)
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// A `<` that starts a tag, comment or end tag (not a lone `<` in text).
fn tag_start(b: &[u8], i: usize) -> bool {
    b[i] == b'<' && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?'))
}

pub fn tokenize(html: &str) -> Vec<Tok> {
    let b = html.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !tag_start(b, i) {
            let start = i;
            i += 1;
            while i < b.len() && !tag_start(b, i) {
                i += 1;
            }
            toks.push(Tok::Text { start, end: i });
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i + 4..].find("-->").map_or(b.len(), |p| i + 4 + p + 3);
            toks.push(Tok::Other);
        } else if b[i + 1] == b'!' || b[i + 1] == b'?' {
            i = html[i..].find('>').map_or(b.len(), |p| i + p + 1);
            toks.push(Tok::Other);
        } else if b[i + 1] == b'/' {
            let start = i;
            let mut j = i + 2;
            let name_start = j;
            while j < b.len() && !is_ws(b[j]) && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let name = html[name_start..j].to_ascii_lowercase();
            i = html[j..].find('>').map_or(b.len(), |p| j + p + 1);
            toks.push(Tok::Close { name, start, end: i });
        } else {
            let (tok, end) = open_tag(html, i);
            i = end;
            let raw_name = match &tok {
                Tok::Open { name, self_closing: false, .. } if RAW.contains(&name.as_str()) => Some(name.clone()),
                _ => None,
            };
            toks.push(tok);
            if let Some(name) = raw_name {
                let close = format!("</{name}");
                let lower = html[i..].to_ascii_lowercase();
                let at = lower.find(&close).map_or(b.len(), |p| i + p);
                if at > i {
                    toks.push(Tok::Raw { name, start: i, end: at });
                }
                i = at;
            }
        }
    }
    toks
}

fn open_tag(html: &str, start: usize) -> (Tok, usize) {
    let b = html.as_bytes();
    let mut i = start + 1;
    let name_start = i;
    while i < b.len() && !is_ws(b[i]) && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    let name = html[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < b.len() && (is_ws(b[i]) || b[i] == b'/') {
            if b[i] == b'/' && b.get(i + 1) == Some(&b'>') {
                self_closing = true;
            }
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'>' {
            i += 1;
            break;
        }
        let an = i;
        while i < b.len() && !is_ws(b[i]) && b[i] != b'=' && b[i] != b'>' && b[i] != b'/' {
            i += 1;
        }
        if i == an {
            // a stray character such as a lone quote: step over it
            i += 1;
            continue;
        }
        let attr_name = html[an..i].to_ascii_lowercase();
        let mut j = i;
        while j < b.len() && is_ws(b[j]) {
            j += 1;
        }
        if j < b.len() && b[j] == b'=' {
            j += 1;
            while j < b.len() && is_ws(b[j]) {
                j += 1;
            }
            if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                let q = b[j];
                let vs = j + 1;
                let ve = html[vs..].find(q as char).map_or(b.len(), |p| vs + p);
                attrs.push(Attr { name: attr_name, value: Some(html[vs..ve].to_string()), range: Some((vs, ve)) });
                i = (ve + 1).min(b.len());
            } else {
                let vs = j;
                while j < b.len() && !is_ws(b[j]) && b[j] != b'>' {
                    j += 1;
                }
                attrs.push(Attr { name: attr_name, value: Some(html[vs..j].to_string()), range: Some((vs, j)) });
                i = j;
            }
        } else {
            attrs.push(Attr { name: attr_name, value: None, range: None });
        }
    }
    (Tok::Open { name, attrs, start, end: i, self_closing }, i)
}

/// The tokens, plus for every token the open tag that holds it.
pub struct Doc<'a> {
    pub html: &'a str,
    pub toks: Vec<Tok>,
    /// For each token: the index of the open tag around it.
    pub parent: Vec<Option<usize>>,
}

impl<'a> Doc<'a> {
    pub fn parse(html: &'a str) -> Self {
        let toks = tokenize(html);
        let mut parent = Vec::with_capacity(toks.len());
        let mut stack: Vec<usize> = Vec::new();
        for (idx, t) in toks.iter().enumerate() {
            parent.push(stack.last().copied());
            match t {
                Tok::Open { name, self_closing, .. } if !self_closing && !is_void(name) => stack.push(idx),
                Tok::Close { name, .. } => {
                    if let Some(pos) = stack.iter().rposition(|&o| matches!(&toks[o], Tok::Open { name: n, .. } if n == name)) {
                        stack.truncate(pos);
                    }
                }
                _ => {}
            }
        }
        Self { html, toks, parent }
    }

    /// The tag name and attributes of an open tag token.
    pub fn element(&self, idx: usize) -> Option<(&str, &[Attr])> {
        match &self.toks[idx] {
            Tok::Open { name, attrs, .. } => Some((name, attrs)),
            _ => None,
        }
    }

    /// The open tags around a token, nearest first.
    pub fn ancestors(&self, idx: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = self.parent[idx];
        while let Some(p) = cur {
            out.push(p);
            cur = self.parent[p];
        }
        out
    }

    pub fn has_ancestor(&self, idx: usize, name: &str) -> bool {
        self.ancestors(idx).into_iter().any(|a| self.element(a).is_some_and(|(n, _)| n == name))
    }

    pub fn attr<'b>(attrs: &'b [Attr], name: &str) -> Option<&'b Attr> {
        attrs.iter().find(|a| a.name == name)
    }

    /// The text of all `<style>` blocks, in order.
    pub fn style_blocks(&self) -> Vec<&'a str> {
        self.toks
            .iter()
            .filter_map(|t| match t {
                Tok::Raw { name, start, end } if name == "style" => Some(&self.html[*start..*end]),
                _ => None,
            })
            .collect()
    }
}

// ---------- entities ----------

/// Decode one entity at the start of `s` (which starts with `&`). Returns the char and the bytes used.
fn entity(s: &str) -> Option<(char, usize)> {
    let end = s.find(';')?;
    if end > 10 {
        return None;
    }
    let body = &s[1..end];
    let ch = if let Some(num) = body.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix(['x', 'X']) { u32::from_str_radix(hex, 16).ok()? } else { num.parse().ok()? };
        char::from_u32(code)?
    } else {
        match body {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            "nbsp" => '\u{a0}',
            "ndash" => '\u{2013}',
            "mdash" => '\u{2014}',
            "rsquo" => '\u{2019}',
            "lsquo" => '\u{2018}',
            "rdquo" => '\u{201d}',
            "ldquo" => '\u{201c}',
            "hellip" => '\u{2026}',
            "copy" => '\u{a9}',
            _ => return None,
        }
    };
    Some((ch, end + 1))
}

pub fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        match entity(rest) {
            Some((c, n)) => {
                out.push(c);
                rest = &rest[n..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

// ---------- Type 2: the image to track ----------

#[derive(Debug, PartialEq)]
pub struct ImgFound {
    /// The `src` as written, decoded.
    pub src: String,
    /// Byte range of the raw `src` value (inside the quotes).
    pub src_range: (usize, usize),
    pub width: Option<String>,
    pub height: Option<String>,
}

fn dimension_is_tiny(v: &Option<String>) -> bool {
    v.as_deref().and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).is_some_and(|n| n <= 1.0)
}

fn candidates<'a>(doc: &'a Doc<'_>) -> impl Iterator<Item = (usize, ImgFound)> + 'a {
    doc.toks.iter().enumerate().filter_map(move |(idx, t)| {
        let Tok::Open { name, attrs, .. } = t else { return None };
        if name != "img" || doc.has_ancestor(idx, "head") {
            return None;
        }
        let src_attr = Doc::attr(attrs, "src")?;
        let (range, raw) = (src_attr.range?, src_attr.value.as_deref()?);
        let width = Doc::attr(attrs, "width").and_then(|a| a.value.clone());
        let height = Doc::attr(attrs, "height").and_then(|a| a.value.clone());
        if dimension_is_tiny(&width) || dimension_is_tiny(&height) {
            return None;
        }
        Some((idx, ImgFound { src: decode(raw).trim().to_string(), src_range: range, width, height }))
    })
}

/// Auto mode: the first image that is not inside a link.
pub fn first_image_outside_links(html: &str) -> Option<ImgFound> {
    let doc = Doc::parse(html);
    candidates(&doc).find(|(idx, f)| !doc.has_ancestor(*idx, "a") && is_web_url(&f.src)).map(|(_, f)| f)
}

/// Manual mode: the image with this `src`.
pub fn image_with_src(html: &str, src: &str) -> Option<ImgFound> {
    let doc = Doc::parse(html);
    let want = src.trim();
    candidates(&doc).find(|(_, f)| f.src == want).map(|(_, f)| f)
}

pub fn is_web_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("http://") || s.starts_with("https://")) && !s.is_empty()
}

/// Swap the `src` of a found image.
pub fn replace_src(html: &str, found: &ImgFound, new_src: &str) -> String {
    let (a, b) = found.src_range;
    format!("{}{}{}", &html[..a], escape_attr(new_src), &html[b..])
}

// ---------- Type 1: the last piece of visible text ----------

#[derive(Clone, Debug)]
struct LChar {
    ch: char,
    node: usize,
    a: usize,
    b: usize,
}

/// A text node that a reader sees: not in the head, not in a comment.
pub fn visible_text_nodes(doc: &Doc<'_>) -> Vec<usize> {
    doc.toks
        .iter()
        .enumerate()
        .filter(|(idx, t)| matches!(t, Tok::Text { .. }) && !doc.has_ancestor(*idx, "head"))
        .map(|(idx, _)| idx)
        .collect()
}

fn logical_chars(doc: &Doc<'_>) -> Vec<LChar> {
    let mut out: Vec<LChar> = Vec::new();
    for idx in visible_text_nodes(doc) {
        let Tok::Text { start, end } = doc.toks[idx] else { continue };
        let raw = &doc.html[start..end];
        let mut pos = 0;
        while pos < raw.len() {
            let rest = &raw[pos..];
            let (ch, used) = if rest.starts_with('&') {
                entity(rest).unwrap_or(('&', 1))
            } else {
                let c = rest.chars().next().unwrap_or(' ');
                (c, c.len_utf8())
            };
            let ch = if ch.is_whitespace() { ' ' } else { ch };
            let (a, b) = (start + pos, start + pos + used);
            // a run of white space inside one node is one character
            match out.last_mut() {
                Some(last) if ch == ' ' && last.ch == ' ' && last.node == idx && last.b == a => last.b = b,
                _ => out.push(LChar { ch, node: idx, a, b }),
            }
            pos += used;
        }
    }
    out
}

/// The needle with white space runs cut to one space.
fn clean_needle(needle: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for c in needle.trim().chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' && out.last() == Some(&' ') {
            continue;
        }
        out.push(c);
    }
    out
}

/// Where the last match sits: the text node and the byte range to cut, for each node it touches.
#[derive(Debug, PartialEq)]
pub struct TextMatch {
    /// `(token index, start, end)` in the order of the email.
    pub spans: Vec<(usize, usize, usize)>,
}

fn match_at(chars: &[LChar], start: usize, needle: &[char]) -> Option<usize> {
    let mut i = start;
    let mut n = 0;
    while n < needle.len() {
        if needle[n] == ' ' {
            if i >= chars.len() || chars[i].ch != ' ' {
                return None;
            }
            // white space in the email may be several characters (one per tag border)
            while i < chars.len() && chars[i].ch == ' ' {
                i += 1;
            }
        } else {
            if i >= chars.len() || chars[i].ch != needle[n] {
                return None;
            }
            i += 1;
        }
        n += 1;
    }
    Some(i)
}

/// The last place where `needle` appears in the visible text. It may run across tags: `Nazmul <b>Hasan</b>`.
pub fn find_last_text(doc: &Doc<'_>, needle: &str) -> Option<TextMatch> {
    let needle = clean_needle(needle);
    if needle.is_empty() {
        return None;
    }
    let chars = logical_chars(doc);
    for start in (0..chars.len()).rev() {
        if chars[start].ch != needle[0] {
            continue;
        }
        if let Some(end) = match_at(&chars, start, &needle) {
            let mut spans: Vec<(usize, usize, usize)> = Vec::new();
            for c in &chars[start..end] {
                match spans.last_mut() {
                    Some(s) if s.0 == c.node => s.2 = c.b,
                    _ => spans.push((c.node, c.a, c.b)),
                }
            }
            return Some(TextMatch { spans });
        }
    }
    None
}

/// Put `replacement` where the match starts and cut the matched text out of every node it touches.
pub fn replace_match(html: &str, m: &TextMatch, replacement: &str) -> String {
    let mut out = String::with_capacity(html.len() + replacement.len());
    let mut pos = 0;
    for (n, (_, a, b)) in m.spans.iter().enumerate() {
        out.push_str(&html[pos..*a]);
        if n == 0 {
            out.push_str(replacement);
        }
        pos = *b;
    }
    out.push_str(&html[pos..]);
    out
}

/// The token index of the text node where the match starts.
pub fn first_node(m: &TextMatch) -> usize {
    m.spans[0].0
}

/// Attributes of all open tags as a map, for tests and for the style reader.
pub fn attr_map(attrs: &[Attr]) -> HashMap<&str, &str> {
    attrs.iter().filter_map(|a| Some((a.name.as_str(), a.value.as_deref()?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(html: &str, needle: &str) -> Option<String> {
        let doc = Doc::parse(html);
        let m = find_last_text(&doc, needle)?;
        Some(replace_match(html, &m, "[IMG]"))
    }

    #[test]
    fn plain_text_is_found() {
        assert_eq!(cut("<p>Hi,</p><p>Nazmul Hasan</p>", "Nazmul Hasan").unwrap(), "<p>Hi,</p><p>[IMG]</p>");
    }

    #[test]
    fn text_split_across_tags_is_found() {
        let r = cut("<p>Best,<br>Nazmul <b>Hasan</b></p>", "Nazmul Hasan").unwrap();
        assert_eq!(r, "<p>Best,<br>[IMG]<b></b></p>");
    }

    #[test]
    fn white_space_between_tags_is_one_space() {
        let r = cut("<div><b>Nazmul</b>\n  <i>Hasan</i></div>", "Nazmul Hasan").unwrap();
        assert!(r.starts_with("<div><b>[IMG]</b>"), "{r}");
        assert!(!r.contains("Hasan") && !r.contains("Nazmul"), "{r}");
    }

    #[test]
    fn the_last_match_wins_not_the_first() {
        let r = cut("<p>Hi, I am Nazmul Hasan and</p><p>-- Nazmul Hasan</p>", "Nazmul Hasan").unwrap();
        assert_eq!(r, "<p>Hi, I am Nazmul Hasan and</p><p>-- [IMG]</p>");
    }

    #[test]
    fn text_in_the_head_or_in_a_comment_does_not_count() {
        assert!(cut("<html><head><title>Nazmul Hasan</title></head><body><!-- Nazmul Hasan --><p>Hi</p></body></html>", "Nazmul Hasan").is_none());
        assert!(cut("<style>.x{content:'Nazmul Hasan'}</style><p>Hi</p>", "Nazmul Hasan").is_none());
    }

    #[test]
    fn not_found_gives_none() {
        assert!(cut("<p>Hello</p>", "Nazmul Hasan").is_none());
        assert!(cut("<p>Hello</p>", "   ").is_none());
    }

    #[test]
    fn entities_are_read_and_the_raw_bytes_are_cut() {
        let r = cut("<p>Tom &amp; Jerry Co</p>", "Tom & Jerry").unwrap();
        assert_eq!(r, "<p>[IMG] Co</p>");
        // a non breaking space counts as a space
        assert!(cut("<p>Nazmul&nbsp;Hasan</p>", "Nazmul Hasan").is_some());
    }

    #[test]
    fn the_rest_of_the_node_stays() {
        assert_eq!(cut("<p>Thanks, Nazmul Hasan!</p>", "Nazmul Hasan").unwrap(), "<p>Thanks, [IMG]!</p>");
    }

    #[test]
    fn a_name_inside_an_attribute_does_not_count() {
        assert!(cut("<a title=\"Nazmul Hasan\" href=\"x\">link</a>", "Nazmul Hasan").is_none());
    }

    #[test]
    fn first_image_skips_links_and_tiny_images() {
        let html = r#"<a href="https://x.com"><img src="https://a.com/in-link.png"></a><img src="https://a.com/pixel.gif" width="1" height="1"><p><img src="https://a.com/logo.png" alt="Logo" width="120"></p>"#;
        let f = first_image_outside_links(html).unwrap();
        assert_eq!(f.src, "https://a.com/logo.png");
        let out = replace_src(html, &f, "https://media.acme.com/p/TOKEN");
        assert!(out.contains(r#"<img src="https://media.acme.com/p/TOKEN" alt="Logo" width="120">"#), "{out}");
        assert!(out.contains("in-link.png"));
    }

    #[test]
    fn no_image_outside_links_gives_none() {
        assert!(first_image_outside_links(r#"<a href="x"><img src="https://a.com/a.png"></a><p>Hi</p>"#).is_none());
        assert!(first_image_outside_links("<p>Hi</p>").is_none());
        // a data: or cid: image cannot be fetched
        assert!(first_image_outside_links(r#"<img src="cid:logo"><img src="data:image/png;base64,AAA=">"#).is_none());
    }

    #[test]
    fn manual_mode_finds_the_exact_src_and_decodes_entities() {
        let html = r#"<img src="https://a.com/a.png?x=1&amp;y=2"><img src="https://a.com/b.png">"#;
        let f = image_with_src(html, "https://a.com/a.png?x=1&y=2").unwrap();
        assert_eq!(f.src_range.0, 10);
        assert!(image_with_src(html, "https://a.com/none.png").is_none());
        let out = replace_src(html, &f, "https://m.acme.com/p/T");
        assert!(out.starts_with(r#"<img src="https://m.acme.com/p/T">"#));
    }

    #[test]
    fn the_tokenizer_keeps_script_and_style_text_raw() {
        let toks = tokenize("<style>p > a { color: red }</style><script>if (a<b) {}</script><p>x</p>");
        assert!(toks.iter().filter(|t| matches!(t, Tok::Raw { .. })).count() == 2);
        assert!(toks.iter().any(|t| matches!(t, Tok::Open { name, .. } if name == "p")));
    }
}
