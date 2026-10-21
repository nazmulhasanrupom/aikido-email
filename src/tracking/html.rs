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

/// A text node that a reader sees: not in the head (the head holds the title and styles).
pub fn visible_text_nodes(doc: &Doc<'_>) -> Vec<usize> {
    doc.toks
        .iter()
        .enumerate()
        .filter(|(idx, t)| matches!(t, Tok::Text { .. }) && !doc.has_ancestor(*idx, "head"))
        .map(|(idx, _)| idx)
        .collect()
}

/// Where the last match sits: the text node and the byte range to cut.
#[derive(Debug, PartialEq)]
pub struct TextMatch {
    /// `(token index, start, end)`. One span: the text must sit in one piece of text, as typed.
    pub spans: Vec<(usize, usize, usize)>,
}

/// The last place where `needle` appears, exactly as typed, in the text of the email (not in tags, comments, the
/// head, `<style>` or `<script>`). If a tag sits in the middle of the text, there is no match.
pub fn find_last_text(doc: &Doc<'_>, needle: &str) -> Option<TextMatch> {
    if needle.is_empty() {
        return None;
    }
    for idx in visible_text_nodes(doc).into_iter().rev() {
        let Tok::Text { start, end } = doc.toks[idx] else { continue };
        if let Some(at) = doc.html[start..end].rfind(needle) {
            return Some(TextMatch { spans: vec![(idx, start + at, start + at + needle.len())] });
        }
    }
    None
}

/// The marks that can be the last character of an email.
pub const END_MARKS: [char; 7] = ['.', ',', '!', '?', ';', ':', '…'];

/// What `find_end_mark` found.
#[derive(Debug, PartialEq)]
pub struct EndMark {
    /// The mark to draw: the last character of the email if it is a mark, else a full stop.
    pub mark: char,
    /// The place to cut (the old mark) or to insert at (a full stop that is added).
    pub at: TextMatch,
    /// The text node the mark sits in or follows. Its style is the style of the mark.
    pub style_node: usize,
    /// `true` when the email already ended with this mark and it is replaced.
    pub replaces: bool,
}

/// Cut trailing white space and `&nbsp;` off a piece of raw text. Returns the length that is left.
fn trimmed_len(raw: &str) -> usize {
    let mut s = raw;
    loop {
        let t = s.trim_end();
        let t = t.strip_suffix("&nbsp;").unwrap_or(t);
        if t.len() == s.len() {
            return t.len();
        }
        s = t;
    }
}

/// The very end of the email. If the last visible character is a mark (`. , ! ? ; : …`), that mark is the place to put
/// the tracker. If it is anything else, a full stop is added right after it (after the link, when the end of the
/// email is the text of a link, so the dot is not part of the link).
pub fn find_end_mark(doc: &Doc<'_>) -> Option<EndMark> {
    let (idx, start, stop) = last_text(doc)?;
    let last = doc.html[start..stop].chars().next_back()?;
    if END_MARKS.contains(&last) {
        let from = stop - last.len_utf8();
        return Some(EndMark { mark: last, at: TextMatch { spans: vec![(idx, from, stop)] }, style_node: idx, replaces: true });
    }
    Some(append_at_end(doc, idx, stop, '.'))
}

/// Always add `mark` at the very end of the email, right after the last visible text (after the link, when the email
/// ends with the text of a link). An existing mark at the end is left as it is.
pub fn find_end_append(doc: &Doc<'_>, mark: char) -> Option<EndMark> {
    let (idx, _, stop) = last_text(doc)?;
    Some(append_at_end(doc, idx, stop, mark))
}

/// The last visible text node that holds something besides white space: `(token, start, end of its last visible character)`.
fn last_text(doc: &Doc<'_>) -> Option<(usize, usize, usize)> {
    for idx in visible_text_nodes(doc).into_iter().rev() {
        let Tok::Text { start, end } = doc.toks[idx] else { continue };
        let len = trimmed_len(&doc.html[start..end]);
        if len > 0 {
            return Some((idx, start, start + len));
        }
    }
    None
}

fn append_at_end(doc: &Doc<'_>, idx: usize, stop: usize, mark: char) -> EndMark {
    let pos = outermost_link_end(doc, idx).unwrap_or(stop);
    EndMark { mark, at: TextMatch { spans: vec![(idx, pos, pos)] }, style_node: idx, replaces: false }
}

/// If the text node is inside a link, the byte position just after the end tag of the outermost link.
fn outermost_link_end(doc: &Doc<'_>, text_idx: usize) -> Option<usize> {
    let link = doc.ancestors(text_idx).into_iter().filter(|&a| doc.element(a).is_some_and(|(n, _)| n == "a")).last()?;
    let mut depth = 0;
    for (i, t) in doc.toks.iter().enumerate().skip(link) {
        match t {
            Tok::Open { name, self_closing: false, .. } if name == "a" => depth += 1,
            Tok::Close { name, end, .. } if name == "a" => {
                depth -= 1;
                if depth == 0 {
                    return Some(*end);
                }
            }
            _ => {}
        }
        let _ = i;
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
    fn text_with_a_tag_in_the_middle_is_not_found() {
        assert!(cut("<p>Best,<br>Nazmul <b>Hasan</b></p>", "Nazmul Hasan").is_none());
        // the same text in one piece is found, even inside a tag around it
        assert_eq!(cut("<p>Best,<br><b>Nazmul Hasan</b></p>", "Nazmul Hasan").unwrap(), "<p>Best,<br><b>[IMG]</b></p>");
    }

    #[test]
    fn the_text_must_match_as_typed() {
        assert!(cut("<p>nazmul hasan</p>", "Nazmul Hasan").is_none());
        assert!(cut("<p>Nazmul  Hasan</p>", "Nazmul Hasan").is_none());
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
        assert!(cut("<p>Hello</p>", "").is_none());
    }

    fn end(html: &str) -> Option<(char, bool, String)> {
        let doc = Doc::parse(html);
        let e = find_end_mark(&doc)?;
        Some((e.mark, e.replaces, replace_match(html, &e.at, "[IMG]")))
    }

    #[test]
    fn a_last_mark_is_replaced() {
        assert_eq!(end("<p>Thanks for your time.</p>").unwrap(), ('.', true, "<p>Thanks for your time[IMG]</p>".to_string()));
        assert_eq!(end("<p>Talk soon!</p>\n").unwrap(), ('!', true, "<p>Talk soon[IMG]</p>\n".to_string()));
        assert_eq!(end("<p>Any questions?&nbsp; </p>").unwrap().0, '?');
        assert_eq!(end("<div>Best,</div>").unwrap(), (',', true, "<div>Best[IMG]</div>".to_string()));
    }

    #[test]
    fn with_no_mark_a_full_stop_is_added_at_the_end() {
        assert_eq!(end("<p>Thanks</p>").unwrap(), ('.', false, "<p>Thanks[IMG]</p>".to_string()));
        // the text node holds the trailing space; the dot goes right after the last letter
        assert_eq!(end("<p>Thanks  </p>").unwrap().2, "<p>Thanks[IMG]  </p>");
    }

    #[test]
    fn the_end_of_the_email_is_the_last_text_not_the_last_tag() {
        let html = "<p>Hi.</p><table><tr><td>Nazmul</td></tr></table><p style=\"x\"><a href=\"u\">unsubscribe here</a></p><!-- end -->";
        let (mark, replaces, out) = end(html).unwrap();
        assert_eq!((mark, replaces), ('.', false));
        // after the link, not inside it
        assert!(out.ends_with("unsubscribe here</a>[IMG]</p><!-- end -->"), "{out}");
    }

    #[test]
    fn a_hyphen_is_always_added_at_the_very_end() {
        let add = |html: &str| {
            let doc = Doc::parse(html);
            let e = find_end_append(&doc, '-')?;
            Some((e.mark, e.replaces, replace_match(html, &e.at, "[IMG]")))
        };
        assert_eq!(add("<p>Thanks.</p>").unwrap(), ('-', false, "<p>Thanks.[IMG]</p>".to_string()));
        assert_eq!(add("<p>Thanks</p><p><a href=\"u\">unsubscribe</a></p>").unwrap().2, "<p>Thanks</p><p><a href=\"u\">unsubscribe</a>[IMG]</p>");
        assert!(add("<html><head></head><body> </body></html>").is_none());
    }

    #[test]
    fn the_end_mark_ignores_the_head_and_empty_text() {
        assert_eq!(end("<html><head><title>x.</title></head><body><p>Hello</p></body></html>").unwrap().2, "<html><head><title>x.</title></head><body><p>Hello[IMG]</p></body></html>");
        assert!(end("<html><head></head><body> </body></html>").is_none());
        assert!(end("<img src=\"https://a.com/a.png\">").is_none());
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
