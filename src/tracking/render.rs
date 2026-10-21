//! Draws a piece of text as a PNG (Type 1). One line, a transparent background, drawn at 2x and shown at 1x.
//!
//! Three font families are bundled in the binary, so the server needs no font on the machine:
//! Liberation Sans (Arial and Helvetica), DejaVu Serif (Georgia and the other serif fonts) and DejaVu Sans
//! (Verdana). An unknown font name gets the closest of the three.
use super::style::{TextStyle, parse_color};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    /// Arial-like and Helvetica-like.
    Sans,
    /// Georgia-like.
    Serif,
    /// Verdana-like (wide).
    Verdana,
}

const SANS: &[&str] = &[
    "arial", "helvetica", "helvetica neue", "liberation sans", "sans-serif", "system-ui", "-apple-system", "blinkmacsystemfont", "segoe ui", "roboto", "calibri", "trebuchet ms", "open sans",
    "lato", "inter", "arimo", "ui-sans-serif",
];
const VERDANA: &[&str] = &["verdana", "tahoma", "dejavu sans", "geneva"];
const SERIF: &[&str] = &["georgia", "times", "times new roman", "serif", "garamond", "cambria", "palatino", "book antiqua", "dejavu serif", "ui-serif", "liberation serif"];

/// The bundled family for a CSS `font-family` list: the first name we know.
pub fn family_for(font: &str) -> Family {
    for name in font.split(',') {
        let n = name.trim().trim_matches(['"', '\'']).trim().to_ascii_lowercase();
        if SANS.contains(&n.as_str()) {
            return Family::Sans;
        }
        if VERDANA.contains(&n.as_str()) {
            return Family::Verdana;
        }
        if SERIF.contains(&n.as_str()) {
            return Family::Serif;
        }
    }
    Family::Sans
}

fn font_bytes(f: Family, bold: bool, italic: bool) -> &'static [u8] {
    match (f, bold, italic) {
        (Family::Sans, false, false) => include_bytes!("fonts/LiberationSans-Regular.ttf"),
        (Family::Sans, true, false) => include_bytes!("fonts/LiberationSans-Bold.ttf"),
        (Family::Sans, false, true) => include_bytes!("fonts/LiberationSans-Italic.ttf"),
        (Family::Sans, true, true) => include_bytes!("fonts/LiberationSans-BoldItalic.ttf"),
        (Family::Serif, false, false) => include_bytes!("fonts/DejaVuSerif.ttf"),
        (Family::Serif, true, false) => include_bytes!("fonts/DejaVuSerif-Bold.ttf"),
        (Family::Serif, false, true) => include_bytes!("fonts/DejaVuSerif-Italic.ttf"),
        (Family::Serif, true, true) => include_bytes!("fonts/DejaVuSerif-BoldItalic.ttf"),
        (Family::Verdana, false, false) => include_bytes!("fonts/DejaVuSans.ttf"),
        (Family::Verdana, true, false) => include_bytes!("fonts/DejaVuSans-Bold.ttf"),
        (Family::Verdana, false, true) => include_bytes!("fonts/DejaVuSans-Oblique.ttf"),
        (Family::Verdana, true, true) => include_bytes!("fonts/DejaVuSans-BoldOblique.ttf"),
    }
}

/// One image per (text, style). Two font names that map to the same bundled font share an image.
pub fn cache_key(text: &str, style: &TextStyle) -> String {
    let id = format!(
        "text-v1|{text}|{:?}|{:.1}|{}|{}|{}|{:.2}|{:?}",
        family_for(&style.font),
        style.size,
        style.color.to_ascii_lowercase(),
        style.bold,
        style.italic,
        style.letter_spacing,
        style.line_height.map(|l| (l * 10.0).round() / 10.0)
    );
    Sha256::digest(id.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
pub struct Rendered {
    pub png: Vec<u8>,
    /// The size to show it at (half the size of the file).
    pub width: u32,
    pub height: u32,
    /// How far the drawn baseline is above the bottom edge, in shown pixels. The `<img>` is moved down by this much
    /// (`vertical-align:-Npx`) so its text sits on the baseline of the text around it.
    pub drop: u32,
}

pub const MAX_TEXT_CHARS: usize = 200;

pub fn render_text(text: &str, style: &TextStyle) -> Result<Rendered, String> {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("The text must have 1 to {MAX_TEXT_CHARS} characters."));
    }
    let size = style.size.clamp(6.0, 72.0);
    let (r, g, b) = parse_color(&style.color).unwrap_or((0x22, 0x22, 0x22));
    let font = FontRef::try_from_slice(font_bytes(family_for(&style.font), style.bold, style.italic)).map_err(|e| format!("font: {e}"))?;
    let scale = PxScale::from(size * 2.0);
    let sc = font.as_scaled(scale);
    let (ascent, descent) = (sc.ascent(), sc.descent());
    let content_h = ascent - descent;
    let spacing = style.letter_spacing.clamp(-5.0, 20.0) * 2.0;

    // layout: the pen position of each glyph
    let mut pen = 0.0f32;
    let mut glyphs = Vec::new();
    let mut prev = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            pen += sc.kern(p, id);
        }
        glyphs.push((id, pen));
        pen += sc.h_advance(id) + spacing;
        prev = Some(id);
    }
    let pad = 2.0; // room for the slant of an italic letter
    let w1 = ((pen.max(1.0) + pad) / 2.0).ceil() as u32;
    let line_h2 = style.line_height.map_or(content_h, |l| (l.clamp(4.0, 200.0) * 2.0).max(content_h));
    let h1 = (line_h2 / 2.0).ceil() as u32;
    let (w2, h2) = (w1 * 2, h1 * 2);
    // the text block sits in the middle of the line
    let baseline = (h2 as f32 - content_h) / 2.0 + ascent;

    let mut alpha = vec![0f32; (w2 * h2) as usize];
    for (id, x) in glyphs {
        let glyph = id.with_scale_and_position(scale, point(x, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let (px, py) = (bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as u32) < w2 && (py as u32) < h2 {
                    let a = &mut alpha[(py as u32 * w2 + px as u32) as usize];
                    *a = 1.0 - (1.0 - *a) * (1.0 - cov);
                }
            });
        }
    }
    // the colour is set in every pixel, so a resized edge does not get a dark fringe
    let mut rgba = Vec::with_capacity(alpha.len() * 4);
    for a in alpha {
        rgba.extend_from_slice(&[r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8]);
    }
    let mut png_bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png_bytes, w2, h2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    }
    let drop = ((h2 as f32 - baseline) / 2.0).round().max(0.0) as u32;
    Ok(Rendered { png: png_bytes, width: w1, height: h1, drop })
}

/// The `<img>` that replaces the text.
pub fn img_tag(url: &str, alt: &str, r: &Rendered) -> String {
    format!(
        "<img src=\"{}\" alt=\"{}\" width=\"{}\" height=\"{}\" style=\"vertical-align:-{}px;border:0\">",
        super::html::escape_attr(url),
        super::html::escape_attr(alt),
        r.width,
        r.height,
        r.drop
    )
}

/// A 1 by 1 transparent PNG, for a token we do not know.
pub fn blank_png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 1, 1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut w) = enc.write_header() {
        let _ = w.write_image_data(&[0, 0, 0, 0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png_bytes: &[u8]) -> (png::OutputInfo, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        (info, buf)
    }

    #[test]
    fn the_text_becomes_a_png_at_twice_the_shown_size() {
        let r = render_text("Nazmul Hasan", &TextStyle::default()).unwrap();
        let (info, pixels) = decode(&r.png);
        assert_eq!((info.width, info.height), (r.width * 2, r.height * 2));
        assert!(r.width > 40 && r.height >= 14, "{r:?}");
        // some pixels are drawn, and the top right corner stays transparent
        assert!(pixels.chunks(4).any(|p| p[3] > 200));
        let corner = &pixels[(info.width as usize - 1) * 4..info.width as usize * 4];
        assert_eq!(corner[3], 0);
        // the colour is #222222 everywhere
        assert!(pixels.chunks(4).all(|p| p[..3] == [0x22, 0x22, 0x22]));
    }

    #[test]
    fn bigger_bold_and_wide_text_makes_a_bigger_image() {
        let small = render_text("Nazmul Hasan", &TextStyle::default()).unwrap();
        let big = render_text("Nazmul Hasan", &TextStyle { size: 28.0, ..TextStyle::default() }).unwrap();
        assert!(big.width > small.width * 3 / 2 && big.height > small.height);
        let bold = render_text("Nazmul Hasan", &TextStyle { bold: true, ..TextStyle::default() }).unwrap();
        assert!(bold.width > small.width);
        let spaced = render_text("Nazmul Hasan", &TextStyle { letter_spacing: 2.0, ..TextStyle::default() }).unwrap();
        assert!(spaced.width > small.width + 15);
        let tall = render_text("Nazmul Hasan", &TextStyle { line_height: Some(40.0), ..TextStyle::default() }).unwrap();
        assert!(tall.height >= 40);
    }

    #[test]
    fn the_baseline_offset_is_a_small_positive_number() {
        let r = render_text("Nazmul Hasan", &TextStyle::default()).unwrap();
        assert!((1..=6).contains(&r.drop), "{r:?}");
    }

    #[test]
    fn font_names_map_to_the_closest_bundled_font() {
        assert_eq!(family_for("Arial, sans-serif"), Family::Sans);
        assert_eq!(family_for("Helvetica Neue"), Family::Sans);
        assert_eq!(family_for("'Georgia', serif"), Family::Serif);
        assert_eq!(family_for("Verdana"), Family::Verdana);
        assert_eq!(family_for("'Some Odd Font', Georgia"), Family::Serif);
        assert_eq!(family_for("Some Odd Font"), Family::Sans);
    }

    #[test]
    fn one_asset_per_text_and_style() {
        let a = TextStyle::default();
        let helvetica = TextStyle { font: "Helvetica".into(), ..a.clone() };
        assert_eq!(cache_key("Nazmul", &a), cache_key("Nazmul", &helvetica), "Arial and Helvetica share one font");
        assert_ne!(cache_key("Nazmul", &a), cache_key("Nazmul H", &a));
        assert_ne!(cache_key("Nazmul", &a), cache_key("Nazmul", &TextStyle { color: "#ff0000".into(), ..a.clone() }));
        assert_ne!(cache_key("Nazmul", &a), cache_key("Nazmul", &TextStyle { bold: true, ..a }));
    }

    #[test]
    fn bad_text_is_refused() {
        assert!(render_text("   ", &TextStyle::default()).is_err());
        assert!(render_text(&"x".repeat(201), &TextStyle::default()).is_err());
    }

    #[test]
    fn the_img_tag_has_size_alt_and_the_shift() {
        let r = Rendered { png: vec![], width: 90, height: 17, drop: 4 };
        let tag = img_tag("https://media.acme.com/i/abc.png", "Nazmul \"Naz\" Hasan", &r);
        assert_eq!(
            tag,
            "<img src=\"https://media.acme.com/i/abc.png\" alt=\"Nazmul &quot;Naz&quot; Hasan\" width=\"90\" height=\"17\" style=\"vertical-align:-4px;border:0\">"
        );
    }

    #[test]
    fn the_blank_png_is_valid() {
        let (info, px) = decode(&blank_png());
        assert_eq!((info.width, info.height, px[3]), (1, 1, 0));
    }
}
