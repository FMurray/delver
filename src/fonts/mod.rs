mod generated;


// use crate::geo::Rect;
use std::collections::HashMap;

use lazy_static::lazy_static;

pub mod canonicalize;
pub use canonicalize::canonicalize_font_name;
pub use generated::*;

// Re-export the generated types
// pub use super::FontMetrics;

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
    pub glyph_widths: HashMap<u32, f32>,
}

// static UNIVERSAL_FALLBACK_FONT_METRICS: Lazy<FontMetrics> = Lazy::new(|| {
//     // Replace with metrics that closely resemble your universal fallback font,
//     // for instance, a metrics set matching DejaVuSans-Bold.
//     FontMetrics {
//         ascent: 683.0,
//         descent: -217.0,
//         cap_height: 662.0,
//         x_height: 450.0,
//         italic_angle: 0.0,
//         bbox: (-168.0, -218.0, 1000.0, 898.0),
//         flags: 0,
//         font_family: "Universal".to_string(),
//         font_weight: "Regular".to_string(),
//         glyph_widths: /* a comprehensive mapping for fallback glyphs */,
//     }
// });

/// Types of font name transformations
enum FontTransform {
    /// Replace the entire name with a different one
    ExactMatch(String, String),
    /// Replace a prefix and keep the rest
    PrefixReplace {
        prefix: String,
        replacement: String,
        /// Optional transforms to apply after prefix replacement
        post_processors: Vec<PostProcessor>,
    },
    /// Apply a custom transformation function
    Custom(fn(&str) -> Option<String>),
}

/// Post-processing operations that can be chained
enum PostProcessor {
    /// Remove a suffix if present
    RemoveSuffix(String),
    /// Map specific variants to canonical names
    MapVariant(HashMap<String, String>),
    /// Strip any leading character (like a dash)
    TrimLeadingChar(char),
}

lazy_static! {
    /// List of transformation rules applied in order
    static ref FONT_TRANSFORMS: Vec<FontTransform> = {
        // Times New Roman variants mapping
        let mut times_variants = HashMap::new();
        times_variants.insert("Bold".to_string(), "Times-Bold".to_string());
        times_variants.insert("BoldItalic".to_string(), "Times-BoldItalic".to_string());
        times_variants.insert("Italic".to_string(), "Times-Italic".to_string());
        times_variants.insert("".to_string(), "Times-Roman".to_string());

        // Arial variants mapping
        let mut arial_variants = HashMap::new();
        arial_variants.insert("Bold".to_string(), "Helvetica-Bold".to_string());
        arial_variants.insert("BoldItalic".to_string(), "Helvetica-BoldItalic".to_string());
        arial_variants.insert("Italic".to_string(), "Helvetica-Oblique".to_string());
        arial_variants.insert("".to_string(), "Helvetica".to_string());

        // Create the transformation rules
        vec![
            // Times New Roman special handling
            FontTransform::PrefixReplace {
                prefix: "TimesNewRomanPS".to_string(),
                replacement: "".to_string(),
                post_processors: vec![
                    PostProcessor::RemoveSuffix("MT".to_string()),
                    PostProcessor::TrimLeadingChar('-'),
                    PostProcessor::MapVariant(times_variants),
                ],
            },

            // Arial special handling
            FontTransform::PrefixReplace {
                prefix: "Arial".to_string(),
                replacement: "".to_string(),
                post_processors: vec![
                    PostProcessor::TrimLeadingChar('-'),
                    PostProcessor::MapVariant(arial_variants),
                ],
            },

            // Example of exact matches
            FontTransform::ExactMatch("CourierNew".to_string(), "Courier".to_string()),
            FontTransform::ExactMatch("CourierNew-Bold".to_string(), "Courier-Bold".to_string()),

            // Add more transformations as needed
        ]
    };
}
