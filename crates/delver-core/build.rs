#[path = "src/fonts/mod.rs"]
mod fonts;

#[path = "src/fonts/canonicalize.rs"]
mod canonicalize;
use canonicalize::canonicalize_font_name;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
#[derive(Debug, Default)]
struct IntermediateMetrics {
    font_name: String,
    font_family: String,
    font_weight: String,
    ascent: f32,
    descent: f32,
    cap_height: f32,
    x_height: f32,
    italic_angle: f32,
    bbox: (f32, f32, f32, f32),
    flags: u32,
    glyph_widths: HashMap<u8, f32>,
}

#[derive(Debug, Clone)]
pub struct FontMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub x_height: f32,
    pub italic_angle: f32,
    pub bbox: (f32, f32, f32, f32),
    pub flags: u32,
    pub font_name: String,
    pub font_family: String,
    pub font_weight: String,
    pub glyph_widths: HashMap<u8, f32>,
}

fn main() {
    let mut output = String::new();
    output.push_str("// AUTO-GENERATED FILE - DO NOT EDIT\n");
    output.push_str("#[allow(unused_imports)]\n");
    output.push_str("use super::FontMetrics;\nuse lazy_static::lazy_static;\nuse std::collections::HashMap;\n\n");

    let afm_dir = Path::new("src/fonts/afm/");
    let mut font_metrics = Vec::new();
    for entry in fs::read_dir(afm_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|s| s != "afm").unwrap_or(true) {
            continue;
        }
        let afm_content = fs::read_to_string(&path).unwrap();
        let metrics = parse_afm(&afm_content);
        font_metrics.push(metrics.clone());
        output.push_str(&generate_lazy_static_block(&metrics));
    }

    let ttf_dir = Path::new("src/fonts/ttf/");
    for entry in fs::read_dir(ttf_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|s| s != "ttf").unwrap_or(true) {
            continue;
        }
        let font_data = fs::read(&path).unwrap();
        println!("Path: {:?}", path);
        let font = rusttype::Font::try_from_vec(font_data).unwrap();

        let scale = rusttype::Scale::uniform(1000.0);
        let v_metrics = font.v_metrics(scale);
        let mut glyph_widths: HashMap<u8, f32> = HashMap::new();
        for code in 32u8..=126u8 {
            let ch = char::from(code);
            let glyph = font.glyph(ch).scaled(scale);
            glyph_widths.insert(code, glyph.h_metrics().advance_width);
        }

        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let (font_family, font_weight) = if name.contains('_') {
            let parts: Vec<&str> = name.split('_').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else if name.contains('-') {
            let parts: Vec<&str> = name.split('-').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (name.clone(), "Regular".to_string())
        };

        let metrics = FontMetrics {
            ascent: v_metrics.ascent,
            descent: v_metrics.descent,
            cap_height: v_metrics.ascent,
            x_height: v_metrics.ascent * 0.5,
            italic_angle: 0.0,
            bbox: (0.0, v_metrics.descent, 1000.0, v_metrics.ascent),
            flags: 0,
            font_name: name.clone(),
            font_family,
            font_weight,
            glyph_widths,
        };
        output.push_str(&generate_lazy_static_block(&metrics));
        font_metrics.push(metrics);
    }

    output.push_str("\nlazy_static! {\n    pub static ref FONT_METRICS: std::collections::HashMap<&'static str, &'static FontMetrics> = {\n        let mut m = std::collections::HashMap::new();\n");
    for font in &font_metrics {
        let canonical_name = canonicalize_font_name(&font.font_name);
        let sanitized_var_name = sanitize_name(&font.font_name);

        output.push_str(&format!(
            "        m.insert(\"{}\", &*{} as &'static FontMetrics);\n",
            canonical_name, sanitized_var_name
        ));
    }
    output.push_str("        m\n    };\n}\n");

    fs::write("src/fonts/generated.rs", output).unwrap();
}

fn generate_lazy_static_block(metrics: &FontMetrics) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "lazy_static! {{\n    pub static ref {}: FontMetrics = FontMetrics {{\n        ascent: {:.1},\n        descent: {:.1},\n        cap_height: {:.1},\n        x_height: {:.1},\n        italic_angle: {:.1},\n        bbox: ({:.1}, {:.1}, {:.1}, {:.1}),\n        flags: {},\n        font_family: \"{}\".to_string(),\n        font_weight: \"{}\".to_string(),\n        glyph_widths: vec![\n",
        sanitize_name(&metrics.font_name),
        metrics.ascent,
        metrics.descent,
        metrics.cap_height,
        metrics.x_height,
        metrics.italic_angle,
        metrics.bbox.0,
        metrics.bbox.1,
        metrics.bbox.2,
        metrics.bbox.3,
        metrics.flags,
        metrics.font_family,
        metrics.font_weight
    ));
    for (code, width) in &metrics.glyph_widths {
        s.push_str(&format!("({}, {:.1}),", code, width));
    }
    s.push_str("].into_iter().collect(),\n    };\n}\n\n");
    s
}

fn parse_afm(content: &str) -> FontMetrics {
    let mut metrics = IntermediateMetrics::default();
    let mut in_char_metrics = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("Comment") {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "FontName" => metrics.font_name = parts[1].trim_matches('"').to_string(),
            "FamilyName" => metrics.font_family = parts[1].trim_matches('"').to_string(),
            "Weight" => metrics.font_weight = parts[1].trim_matches('"').to_string(),
            "ItalicAngle" => metrics.italic_angle = parts[1].parse().unwrap_or(0.0),
            "IsFixedPitch" => {
                if parts[1] == "true" {
                    metrics.flags |= 1 << 0
                }
            }
            "FontBBox" => {
                if parts.len() >= 5 {
                    metrics.bbox = (
                        parts[1].parse().unwrap_or(0.0),
                        parts[2].parse().unwrap_or(0.0),
                        parts[3].parse().unwrap_or(0.0),
                        parts[4].parse().unwrap_or(0.0),
                    );
                }
            }
            "CapHeight" => metrics.cap_height = parts[1].parse().unwrap_or(0.0),
            "Ascender" => metrics.ascent = parts[1].parse().unwrap_or(0.0),
            "Descender" => metrics.descent = parts[1].parse().unwrap_or(0.0),
            "XHeight" => metrics.x_height = parts[1].parse().unwrap_or(0.0),
            "StartCharMetrics" => in_char_metrics = true,
            "EndCharMetrics" => in_char_metrics = false,
            "C" if in_char_metrics => {
                let mut code: i32 = -1;
                let mut width = 0.0;

                let components: Vec<&str> = line.split(';').collect();
                for component in components {
                    let fields: Vec<&str> = component.split_whitespace().collect();
                    match fields.as_slice() {
                        ["C", c] => code = c.parse().unwrap_or(-1),
                        ["WX", w] => width = w.parse().unwrap_or(0.0),
                        _ => {}
                    }
                }

                if code >= 0 && code <= 255 {
                    metrics.glyph_widths.insert(code as u8, width);
                }
            }
            _ => {}
        }
    }

    if metrics.italic_angle != 0.0 {
        metrics.flags |= 1 << 6;
    }

    FontMetrics {
        ascent: metrics.ascent,
        descent: metrics.descent,
        cap_height: metrics.cap_height,
        x_height: metrics.x_height,
        italic_angle: metrics.italic_angle,
        bbox: metrics.bbox,
        flags: metrics.flags,
        font_name: metrics.font_name,
        font_family: metrics.font_family,
        font_weight: metrics.font_weight,
        glyph_widths: metrics.glyph_widths,
    }
}

fn sanitize_name(name: &str) -> String {
    name.replace('-', "_").replace(' ', "_")
}
