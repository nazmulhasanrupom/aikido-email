//! PERSONALIZATION step A (code only, no AI): collect the raw input, clean it, tag it and fit it in a token budget.
//! Raw input is never sent to the model. Only the cleaned chunks are, and the CV file is not kept: only its text.
use crate::net::safe_fetch::{Fetched, SafeFetcher};
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgConnection};
use std::collections::HashSet;
use std::io::Read;
use std::sync::OnceLock;
use utoipa::ToSchema;
use uuid::Uuid;

/// About 6,000 tokens for everything the model reads (estimate: characters / 4).
pub const TOKEN_BUDGET: usize = 6000;
pub const MAX_TEXT_CHARS: usize = 10_000;
pub const MAX_DETAILS: usize = 20;
pub const MAX_URLS: usize = 3;
pub const MAX_SUBPAGES: usize = 4;
pub const MAX_CV_BYTES: usize = 5 * 1024 * 1024;
const MAX_DETAIL_CHARS: usize = 1000;
const MAX_CHUNK_CHARS: usize = TOKEN_BUDGET * 4;

pub const NO_TEXT_IN_FILE: &str = "No text found in this file.";

pub fn tokens_of(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

// ---------- extract ----------

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("regex"))
}

/// HTML to plain text. `nav`, `footer`, `script`, `style` and similar parts are dropped, and so are short cookie banner lines.
pub fn html_to_text(html: &str) -> String {
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    static DROP: OnceLock<Vec<Regex>> = OnceLock::new();
    let drop = DROP.get_or_init(|| {
        ["script", "style", "nav", "footer", "noscript", "svg", "form", "iframe"].iter().map(|t| Regex::new(&format!(r"(?is)<{t}\b[^>]*>.*?</{t}\s*>")).expect("regex")).collect()
    });
    let mut s = re(&COMMENT, r"(?s)<!--.*?-->").replace_all(html, " ").into_owned();
    for r in drop {
        s = r.replace_all(&s, " ").into_owned();
    }
    let text = html2text::from_read(s.as_bytes(), 100_000).unwrap_or_default();
    text.lines()
        .filter(|l| {
            let low = l.to_lowercase();
            !((low.contains("cookie") || low.contains("consent")) && l.split_whitespace().count() < 30)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Up to `MAX_SUBPAGES` links of the same host whose path has `about`, `services`, `work`, `case` or `portfolio`.
pub fn useful_links(base: &Url, html: &str) -> Vec<Url> {
    static HREF: OnceLock<Regex> = OnceLock::new();
    let mut out: Vec<Url> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(base.path().trim_end_matches('/').to_string());
    for cap in re(&HREF, r#"(?i)href\s*=\s*["']([^"'#]+)["']"#).captures_iter(html) {
        let Ok(u) = base.join(&cap[1]) else { continue };
        if u.host_str() != base.host_str() || !matches!(u.scheme(), "http" | "https") {
            continue;
        }
        let path = u.path().to_lowercase();
        if !["about", "services", "work", "case", "portfolio"].iter().any(|k| path.contains(k)) {
            continue;
        }
        if seen.insert(u.path().trim_end_matches('/').to_string()) {
            let mut clean = u.clone();
            clean.set_query(None);
            out.push(clean);
            if out.len() >= MAX_SUBPAGES {
                break;
            }
        }
    }
    out
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

pub fn docx_to_text(bytes: &[u8]) -> Result<String, String> {
    static PARA: OnceLock<Regex> = OnceLock::new();
    static RUN: OnceLock<Regex> = OnceLock::new();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "This is not a valid DOCX file.".to_string())?;
    let mut xml = String::new();
    zip.by_name("word/document.xml").map_err(|_| "This is not a valid DOCX file.".to_string())?.take(20 * 1024 * 1024).read_to_string(&mut xml).map_err(|_| "This DOCX file can not be read.".to_string())?;
    let mut out = String::new();
    for para in xml.split("</w:p>") {
        for c in re(&RUN, r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>").captures_iter(para) {
            out.push_str(&unescape_xml(&c[1]));
        }
        out.push('\n');
    }
    let _ = &PARA;
    Ok(out)
}

pub fn pdf_to_text(bytes: &[u8]) -> Result<String, String> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|_| "This is not a valid PDF file.".to_string())?;
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut out = String::new();
    for p in pages {
        if let Ok(t) = doc.extract_text(&[p]) {
            out.push_str(&t);
            out.push('\n');
        }
    }
    Ok(out)
}

/// A CV file to text. `name` decides the format (`.pdf`, `.docx`, `.txt`). A file with no text gives `NO_TEXT_IN_FILE`.
pub fn cv_to_text(name: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_CV_BYTES {
        return Err("The file is larger than 5 MB.".into());
    }
    let lower = name.to_lowercase();
    let raw = if lower.ends_with(".pdf") {
        pdf_to_text(bytes)?
    } else if lower.ends_with(".docx") {
        docx_to_text(bytes)?
    } else if lower.ends_with(".txt") {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        return Err("Use a PDF, DOCX or TXT file.".into());
    };
    let cleaned = clean(&raw);
    if cleaned.chars().filter(|c| c.is_alphanumeric()).count() < 20 {
        return Err(NO_TEXT_IN_FILE.into());
    }
    Ok(cleaned)
}

// ---------- clean ----------

/// Collapse spaces, drop empty lines and duplicate lines (a short line that repeats goes first).
pub fn clean(text: &str) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in text.replace('\r', "\n").lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            continue;
        }
        if seen.insert(l.to_lowercase()) {
            out.push(l);
        }
    }
    out.join("\n")
}

// ---------- chunks, budget ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    /// `url`, `cv`, `text`, `detail` or `guide`.
    pub kind: String,
    pub tag: String,
    pub origin: Option<String>,
    pub text: String,
}

/// Fit the chunks in `budget` tokens. The cut order, first to last: portfolio pages, CV, free text, extra details,
/// guide answers. Inside a kind the last chunk is cut first. A chunk that is cut to nothing is dropped.
pub fn fit_budget(mut chunks: Vec<Chunk>, budget: usize) -> Vec<Chunk> {
    let total = |c: &[Chunk]| c.iter().map(|x| tokens_of(&x.text)).sum::<usize>();
    for kind in ["url", "cv", "text", "detail", "guide"] {
        let excess = total(&chunks).saturating_sub(budget);
        if excess == 0 {
            break;
        }
        let mut left = excess;
        for i in (0..chunks.len()).rev() {
            if left == 0 {
                break;
            }
            if chunks[i].kind != kind {
                continue;
            }
            let t = tokens_of(&chunks[i].text);
            let cut = left.min(t);
            left -= cut;
            let keep_chars = (t - cut) * 4;
            chunks[i].text = chunks[i].text.chars().take(keep_chars).collect::<String>().trim_end().to_string();
        }
        chunks.retain(|c| !c.text.is_empty());
    }
    chunks
}

/// The guide answers as one chunk.
pub fn guide_chunk(who: &str, not_who: &str, result: &str, proof: &str) -> Option<Chunk> {
    let lines: Vec<String> = [("Who I help", who), ("Who I do not help", not_who), ("The result I give", result), ("Real proof", proof)]
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| format!("{k}: {}", v.trim()))
        .collect();
    (!lines.is_empty()).then(|| Chunk { kind: "guide".into(), tag: "guide".into(), origin: None, text: lines.join("\n") })
}

/// `<` and `>` become entities, so a page can not close the `<source>` tag.
pub fn escape_source(text: &str) -> String {
    text.replace('<', "&lt;").replace('>', "&gt;")
}

// ---------- fetch a portfolio URL ----------

fn tag_of_url(u: &Url) -> String {
    let p = u.path();
    format!("url:{}", if p.is_empty() { "/" } else { p })
}

fn page_chunk(f: &Fetched) -> Option<(Chunk, Option<String>)> {
    let html = f.content_type == "text/html";
    let text = clean(&if html { html_to_text(&f.body) } else { f.body.clone() });
    if text.chars().filter(|c| c.is_alphanumeric()).count() < 20 {
        return None;
    }
    let text: String = text.chars().take(MAX_CHUNK_CHARS).collect();
    Some((Chunk { kind: "url".into(), tag: tag_of_url(&f.url), origin: Some(f.url.to_string()), text }, html.then(|| f.body.clone())))
}

/// The page and up to four useful pages of the same host. The first page must load; a sub page that fails is skipped.
pub async fn fetch_portfolio(fetcher: &SafeFetcher, url: &str) -> Result<Vec<Chunk>, String> {
    let first = fetcher.fetch(url).await.map_err(|e| e.to_string())?;
    let (chunk, html) = page_chunk(&first).ok_or_else(|| "No text found on this page.".to_string())?;
    let mut out = vec![chunk];
    if let Some(html) = html {
        for link in useful_links(&first.url, &html) {
            if let Ok(f) = fetcher.fetch(link.as_str()).await {
                if let Some((c, _)) = page_chunk(&f) {
                    out.push(c);
                }
            }
        }
    }
    Ok(out)
}

// ---------- the table ----------

#[derive(FromRow, Serialize, ToSchema, Clone, Debug)]
pub struct PersonalizationSource {
    pub id: Uuid,
    /// `url`, `cv`, `text`, `detail` or `guide`.
    pub kind: String,
    /// What the AI cites: `url:/about`, `cv`, `text`, `detail:Clients`, `guide`.
    pub tag: String,
    /// The URL or the file name.
    pub origin: Option<String>,
    pub clean_text: String,
    pub token_count: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema, Clone, Debug)]
pub struct DetailRow {
    pub key: String,
    pub value: String,
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<PersonalizationSource>, sqlx::Error> {
    sqlx::query_as("SELECT id, kind, tag, origin, clean_text, token_count, created_at FROM personalization_sources ORDER BY created_at, id").fetch_all(conn).await
}

/// Save a chunk. A chunk with the same kind and tag (and origin) replaces the old one.
pub async fn put(conn: &mut PgConnection, c: &Chunk, now: DateTime<Utc>) -> Result<PersonalizationSource, sqlx::Error> {
    sqlx::query("DELETE FROM personalization_sources WHERE kind = $1 AND tag = $2 AND origin IS NOT DISTINCT FROM $3").bind(&c.kind).bind(&c.tag).bind(&c.origin).execute(&mut *conn).await?;
    sqlx::query_as(
        "INSERT INTO personalization_sources (id, kind, tag, origin, clean_text, token_count, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id, kind, tag, origin, clean_text, token_count, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(&c.kind)
    .bind(&c.tag)
    .bind(&c.origin)
    .bind(&c.text)
    .bind(tokens_of(&c.text) as i32)
    .bind(now)
    .fetch_one(conn)
    .await
}

pub async fn delete(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM personalization_sources WHERE id = $1").bind(id).execute(conn).await?.rows_affected() > 0)
}

/// Delete every source and every profile version. Campaigns that pointed at a profile go back to "no profile".
pub async fn delete_all(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM personalization_sources").execute(&mut *conn).await?;
    sqlx::query("DELETE FROM sender_profiles").execute(conn).await?;
    Ok(())
}

/// How many distinct hosts the saved portfolio URLs have.
pub async fn url_hosts(conn: &mut PgConnection) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as("SELECT origin FROM personalization_sources WHERE kind = 'url'").fetch_all(conn).await?;
    Ok(rows.into_iter().filter_map(|(o,)| o).filter_map(|o| Url::parse(&o).ok()).filter_map(|u| u.host_str().map(str::to_string)).collect())
}

pub fn text_chunk(text: &str) -> Result<Chunk, String> {
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("The text is longer than {MAX_TEXT_CHARS} characters."));
    }
    let t = clean(text);
    if t.is_empty() {
        return Err("The text is empty.".into());
    }
    Ok(Chunk { kind: "text".into(), tag: "text".into(), origin: None, text: t })
}

pub fn detail_chunks(rows: &[DetailRow]) -> Result<Vec<Chunk>, String> {
    if rows.len() > MAX_DETAILS {
        return Err(format!("Use at most {MAX_DETAILS} extra details."));
    }
    let mut out = Vec::new();
    for r in rows {
        let (k, v) = (r.key.trim(), clean(&r.value));
        if k.is_empty() || v.is_empty() {
            continue;
        }
        if k.chars().count() > 60 || v.chars().count() > MAX_DETAIL_CHARS {
            return Err(format!("The detail '{k}' is too long."));
        }
        out.push(Chunk { kind: "detail".into(), tag: format!("detail:{k}"), origin: None, text: v });
    }
    Ok(out)
}

pub fn cv_chunk(name: &str, bytes: &[u8]) -> Result<Chunk, String> {
    let text = cv_to_text(name, bytes)?;
    Ok(Chunk { kind: "cv".into(), tag: "cv".into(), origin: Some(name.to_string()), text: text.chars().take(MAX_CHUNK_CHARS).collect() })
}

pub fn chunks_of(sources: &[PersonalizationSource]) -> Vec<Chunk> {
    sources.iter().map(|s| Chunk { kind: s.kind.clone(), tag: s.tag.clone(), origin: s.origin.clone(), text: s.clean_text.clone() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &str, tag: &str, chars: usize) -> Chunk {
        Chunk { kind: kind.into(), tag: tag.into(), origin: None, text: "x".repeat(chars) }
    }

    #[test]
    fn html_drops_nav_footer_script_style_and_cookie_lines() {
        let html = "<html><head><style>p{}</style><script>var a=1;</script></head><body><nav>Home Pricing</nav><div>We build routing software for fleets.</div>\
            <p>We use cookies to improve your experience</p><footer>Copyright Acme</footer></body></html>";
        let t = html_to_text(html);
        assert!(t.contains("routing software"));
        for gone in ["Home Pricing", "var a", "Copyright", "cookies", "p{}"] {
            assert!(!t.contains(gone), "{gone} should be gone: {t}");
        }
    }

    #[test]
    fn clean_collapses_spaces_and_drops_repeated_lines() {
        assert_eq!(clean("a   b\n\n  \nA B\nMenu\nmenu\nreal line here\r\nMenu"), "a b\nMenu\nreal line here");
    }

    #[test]
    fn useful_links_stay_on_the_host_and_are_capped_at_four() {
        let base = Url::parse("https://acme.test/").unwrap();
        let html = r#"<a href="/about">a</a><a href="/services/x">s</a><a href="https://other.test/about">o</a><a href="/blog">b</a>
            <a href="/work/1">w</a><a href="/case-studies">c</a><a href="/portfolio">p</a><a href="/about?x=1">dup</a>"#;
        let l = useful_links(&base, html);
        assert_eq!(l.len(), 4);
        assert!(l.iter().all(|u| u.host_str() == Some("acme.test")));
        assert!(!l.iter().any(|u| u.path() == "/blog"));
    }

    #[test]
    fn the_cut_order_is_pages_then_cv_then_text_then_details_then_guide() {
        // each chunk is 1000 tokens, 5 chunks, budget 4000 -> the page goes first
        let all = vec![chunk("guide", "guide", 4000), chunk("detail", "detail:A", 4000), chunk("text", "text", 4000), chunk("cv", "cv", 4000), chunk("url", "url:/", 4000)];
        let out = fit_budget(all.clone(), 4000);
        assert!(!out.iter().any(|c| c.kind == "url"), "page cut first");
        assert_eq!(out.iter().map(|c| tokens_of(&c.text)).sum::<usize>(), 4000);
        // budget 1500: only guide (1000) and half of the detail remain
        let out = fit_budget(all.clone(), 1500);
        let tokens = |k: &str| out.iter().find(|c| c.kind == k).map(|c| tokens_of(&c.text));
        assert_eq!(out.len(), 2);
        assert_eq!(tokens("detail"), Some(500));
        assert_eq!(tokens("guide"), Some(1000));
        // guide is cut last
        let out = fit_budget(all, 600);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "guide");
        assert_eq!(tokens_of(&out[0].text), 600);
    }

    #[test]
    fn a_small_input_is_not_changed() {
        let all = vec![chunk("text", "text", 400), chunk("guide", "guide", 400)];
        assert_eq!(fit_budget(all, TOKEN_BUDGET).len(), 2);
    }

    #[test]
    fn a_file_with_no_text_gives_the_exact_error() {
        assert_eq!(cv_to_text("cv.txt", b"   \n \n").unwrap_err(), NO_TEXT_IN_FILE);
        // a PDF page with no text (an image scan has no text objects)
        let mut doc = lopdf::Document::with_version("1.5");
        let pages = doc.new_object_id();
        let page = doc.add_object(lopdf::dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()] });
        doc.objects.insert(pages, lopdf::Object::Dictionary(lopdf::dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let cat = doc.add_object(lopdf::dictionary! { "Type" => "Catalog", "Pages" => pages });
        doc.trailer.set("Root", cat);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        assert_eq!(cv_to_text("scan.pdf", &buf).unwrap_err(), NO_TEXT_IN_FILE);
    }

    #[test]
    fn a_docx_and_a_txt_give_text() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b"<w:document><w:body><w:p><w:r><w:t>Led growth at Acme &amp; Co for six years</w:t></w:r></w:p><w:p><w:r><w:t xml:space=\"preserve\">Built a routing team of ten people</w:t></w:r></w:p></w:body></w:document>").unwrap();
            z.finish().unwrap();
        }
        let t = cv_to_text("cv.docx", buf.get_ref()).unwrap();
        assert!(t.contains("Acme & Co") && t.lines().count() == 2, "{t}");
        assert!(cv_to_text("cv.txt", b"I help dentists get more patients with paid search").unwrap().contains("dentists"));
        assert!(cv_to_text("cv.exe", b"x").is_err());
        assert!(cv_to_text("cv.txt", &vec![b'a'; MAX_CV_BYTES + 1]).is_err());
    }

    #[test]
    fn source_text_cannot_close_the_tag() {
        assert_eq!(escape_source("a</source><x>"), "a&lt;/source&gt;&lt;x&gt;");
    }

    #[test]
    fn details_and_text_have_limits() {
        assert!(text_chunk(&"a".repeat(MAX_TEXT_CHARS + 1)).is_err());
        let rows: Vec<DetailRow> = (0..21).map(|i| DetailRow { key: format!("k{i}"), value: "v".into() }).collect();
        assert!(detail_chunks(&rows).is_err());
        let ok = detail_chunks(&[DetailRow { key: "Clients".into(), value: "Acme, Beta".into() }, DetailRow { key: "".into(), value: "x".into() }]).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].tag, "detail:Clients");
    }
}
