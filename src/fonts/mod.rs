mod generated;

pub use generated::*;

use std::collections::HashMap;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FontMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub x_height: f32,
    pub italic_angle: f32,
    pub bbox: (f32, f32, f32, f32),
    pub flags: u32,
    pub font_family: String,
    pub font_weight: String,
    pub glyph_widths: HashMap<u8, f32>,
}

// Existing sanitization needs to handle PDF's subset prefixes
pub fn sanitize_font_name(raw_name: &str) -> &str {
    // Remove common PostScript suffixes
    let cleaned = raw_name
        .strip_suffix("PSMT")
        .unwrap_or(raw_name)
        .strip_suffix("MT")
        .unwrap_or(raw_name)
        .strip_suffix("PS")
        .unwrap_or(raw_name)
        .split('-')
        .next()
        .unwrap()
        .split('+')
        .last()
        .unwrap();

    // Handle Times New Roman naming variations
    if cleaned.starts_with("TimesNewRoman") {
        return match cleaned.trim_start_matches("TimesNewRoman") {
            "Bold" => "Times-Bold",
            "Italic" => "Times-Italic",
            "BoldItalic" => "Times-BoldItalic",
            _ => "Times-Roman",
        };
    }

    // Fallback for other fonts
    match cleaned {
        "Arial" => "Helvetica",
        "ArialBold" => "Helvetica-Bold",
        "CourierNew" => "Courier",
        _ => cleaned,
    }
}
