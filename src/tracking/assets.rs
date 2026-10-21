//! The images the tracking server sends back. One asset per (text, style) for Type 1 and one per source URL for
//! Type 2, so the server never draws or downloads one image per lead.
use super::render::{self, Rendered};
use super::style::TextStyle;
use crate::state::AppState;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(FromRow, Debug, Clone, Serialize, ToSchema)]
pub struct Asset {
    pub id: Uuid,
    pub kind: String,
    pub content_type: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub baseline_drop: Option<i32>,
    pub source_url: Option<String>,
}

const COLS: &str = "id, kind, content_type, width, height, baseline_drop, source_url";

/// The cache key of a Type 2 image.
pub fn source_key(url: &str) -> String {
    Sha256::digest(format!("sig-v1|{}", url.trim()).as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The image of a text and a style. Drawn the first time, then read from the database.
pub async fn text_asset(st: &AppState, text: &str, style: &TextStyle) -> Result<Asset, String> {
    let key = render::cache_key(text, style);
    if let Some(a) = find(st, "text_image", &key).await? {
        return Ok(a);
    }
    let r = render::render_text(text, style)?;
    sqlx::query(
        "INSERT INTO tracking_assets (id, kind, cache_key, file, content_type, width, height, baseline_drop, created_at, updated_at)
         VALUES ($1, 'text_image', $2, $3, 'image/png', $4, $5, $6, $7, $7) ON CONFLICT (kind, cache_key) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(&key)
    .bind(&r.png)
    .bind(r.width as i32)
    .bind(r.height as i32)
    .bind(r.drop as i32)
    .bind(st.now())
    .execute(&st.pool)
    .await
    .map_err(|e| e.to_string())?;
    find(st, "text_image", &key).await?.ok_or_else(|| "the image was not saved".to_string())
}

/// The numbers to build the `<img>` tag of an asset.
pub fn as_rendered(a: &Asset) -> Rendered {
    Rendered { png: Vec::new(), width: a.width.unwrap_or(0).max(0) as u32, height: a.height.unwrap_or(0).max(0) as u32, drop: a.baseline_drop.unwrap_or(0).max(0) as u32 }
}

async fn find(st: &AppState, kind: &str, key: &str) -> Result<Option<Asset>, String> {
    sqlx::query_as(&format!("SELECT {COLS} FROM tracking_assets WHERE kind = $1 AND cache_key = $2")).bind(kind).bind(key).fetch_optional(&st.pool).await.map_err(|e| e.to_string())
}

/// The size of a PNG, GIF or JPEG, read from its header. `None` for anything else.
pub fn sniff_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }
    if bytes.len() > 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((w, h));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            i += 2 + len.max(2);
        }
    }
    None
}

/// The stored copy of an image at a URL. Downloaded the first time. `refresh` downloads it again and keeps the
/// asset id, so the tokens of emails that are already sent still show the new image.
pub async fn signature_asset(st: &AppState, url: &str, refresh: bool) -> Result<Asset, String> {
    let url = url.trim();
    let key = source_key(url);
    if !refresh {
        if let Some(a) = find(st, "signature_image", &key).await? {
            return Ok(a);
        }
    }
    let got = st.fetcher.fetch_image(url).await.map_err(|e| e.to_string())?;
    let (w, h) = sniff_size(&got.bytes).map_or((None, None), |(w, h)| (Some(w as i32), Some(h as i32)));
    let content_type = if got.content_type == "image/jpg" { "image/jpeg".to_string() } else { got.content_type };
    sqlx::query(
        "INSERT INTO tracking_assets (id, kind, cache_key, file, content_type, width, height, source_url, created_at, updated_at)
         VALUES ($1, 'signature_image', $2, $3, $4, $5, $6, $7, $8, $8)
         ON CONFLICT (kind, cache_key) DO UPDATE SET file = EXCLUDED.file, content_type = EXCLUDED.content_type, width = EXCLUDED.width, height = EXCLUDED.height, updated_at = EXCLUDED.updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(&key)
    .bind(&got.bytes)
    .bind(&content_type)
    .bind(w)
    .bind(h)
    .bind(url)
    .bind(st.now())
    .execute(&st.pool)
    .await
    .map_err(|e| e.to_string())?;
    find(st, "signature_image", &key).await?.ok_or_else(|| "the image was not saved".to_string())
}

/// The bytes of an asset, for the tracking server and the preview.
pub async fn file(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<(Vec<u8>, String)>, sqlx::Error> {
    sqlx::query_as("SELECT file, content_type FROM tracking_assets WHERE id = $1").bind(id).fetch_optional(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_is_read_from_the_header() {
        let png = render::render_text("Hi", &TextStyle::default()).unwrap();
        let (w, h) = sniff_size(&png.png).unwrap();
        assert_eq!((w, h), (png.width * 2, png.height * 2));
        assert_eq!(sniff_size(b"GIF89a\x10\x00\x20\x00rest...."), Some((16, 32)));
        // a JPEG: SOI, an APP0 segment, then SOF0 with height 0x0030 and width 0x0040
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x30, 0x00, 0x40, 0x03, 0x01, 0x00];
        assert_eq!(sniff_size(&jpeg), Some((64, 48)));
        assert_eq!(sniff_size(b"not an image at all, really not one"), None);
    }

    #[test]
    fn the_source_key_depends_on_the_url() {
        assert_eq!(source_key("https://a.com/x.png"), source_key(" https://a.com/x.png "));
        assert_ne!(source_key("https://a.com/x.png"), source_key("https://a.com/y.png"));
    }
}
