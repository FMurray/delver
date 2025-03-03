use lazy_static::lazy_static;
use std::collections::HashMap;

/// Types of font name transformations
pub enum FontTransform {
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
pub enum PostProcessor {
    /// Remove a suffix if present
    RemoveSuffix(String),
    /// Map specific variants to canonical names
    MapVariant(HashMap<String, String>),
    /// Strip any leading character (like a dash)
    TrimLeadingChar(char),
}

lazy_static! {
    /// List of transformation rules applied in order
    pub static ref FONT_TRANSFORMS: Vec<FontTransform> = {
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

/// Returns the canonical font name used as lookup key by applying transformation rules.
pub fn canonicalize_font_name(raw: &str) -> String {
    // Try each transformation in order
    for transform in FONT_TRANSFORMS.iter() {
        match transform {
            FontTransform::ExactMatch(from, to) => {
                if raw == from {
                    return to.clone();
                }
            }

            FontTransform::PrefixReplace {
                prefix,
                replacement,
                post_processors,
            } => {
                if raw.starts_with(prefix.as_str()) {
                    // Apply the prefix replacement
                    let mut result = if replacement.is_empty() {
                        raw[prefix.len()..].to_string()
                    } else {
                        format!("{}{}", replacement, &raw[prefix.len()..])
                    };

                    // Apply all post-processors in sequence
                    for processor in post_processors {
                        match processor {
                            PostProcessor::RemoveSuffix(suffix) => {
                                if result.ends_with(suffix.as_str()) {
                                    result = result[..result.len() - suffix.len()].to_string();
                                }
                            }
                            PostProcessor::TrimLeadingChar(c) => {
                                // Fix: use a reference to c for Pattern trait
                                result = result.trim_start_matches(*c).to_string();
                            }
                            PostProcessor::MapVariant(variants) => {
                                if let Some(mapped) = variants.get(&result) {
                                    return mapped.clone();
                                }
                            }
                        }
                    }
                    return result;
                }
            }

            FontTransform::Custom(func) => {
                if let Some(result) = func(raw) {
                    return result;
                }
            }
        }
    }

    // If no transformation matched, return the original name
    raw.to_string()
}

// Example custom transformation function
pub fn _handle_symbol_fonts(name: &str) -> Option<String> {
    if name.contains("Symbol") || name.contains("Zapf") {
        // Symbol fonts often need special handling
        Some(name.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_fonts() {
        // Times fonts
        assert_eq!(
            canonicalize_font_name("TimesNewRomanPS-BoldMT"),
            "Times-Bold"
        );
        assert_eq!(
            canonicalize_font_name("TimesNewRomanPS-ItalicMT"),
            "Times-Italic"
        );

        // Arial fonts
        assert_eq!(canonicalize_font_name("Arial-Bold"), "Helvetica-Bold");
        assert_eq!(canonicalize_font_name("Arial"), "Helvetica");

        // Exact matches
        assert_eq!(canonicalize_font_name("CourierNew"), "Courier");

        // Non-matching fonts are preserved
        assert_eq!(canonicalize_font_name("DejaVuSans-Bold"), "DejaVuSans-Bold");
    }
}
