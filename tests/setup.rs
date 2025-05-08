use lopdf::content::{Content, Operation};
use lopdf::dictionary;
use lopdf::{Document, Object, Stream};
use std::path::Path;
pub struct PdfConfig {
    pub title: String,
    pub sections: Vec<Section>,
    pub font_name: String,
    pub title_font_size: f32,
    pub heading_font_size: f32,
    pub body_font_size: f32,
    pub output_path: String,
}

pub struct Section {
    pub heading: String,
    pub content: String,
}

impl Default for PdfConfig {
    fn default() -> Self {
        PdfConfig {
            title: "Hello World!".to_string(),
            sections: vec![
                Section {
                    heading: "Subheading 1".to_string(),
                    content: "This is the first section text.".to_string(),
                },
                Section {
                    heading: "Subheading 2".to_string(),
                    content: "This is the second section text.".to_string(),
                },
            ],
            font_name: "Courier".to_string(),
            title_font_size: 48.0,
            heading_font_size: 24.0,
            body_font_size: 12.0,
            output_path: "tests/example.pdf".to_string(),
        }
    }
}

pub fn create_test_pdf_with_config(config: PdfConfig) -> Result<(), std::io::Error> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => config.font_name.clone(),
    });

    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    // Build operations vector
    let mut operations = vec![];

    // Use US Letter page dimensions (612.0 x 792.0) to match test expectations
    let page_width = 612.0; // US Letter width in points (matches test assertions)
    let _page_height = 792.0; // US Letter height in points
    let x_center = page_width / 2.0;
    
    // More accurate width calculation for Helvetica - based on actual widths
    // Helvetica average character width is roughly 0.3-0.4 * font size
    let approx_width_per_char = match config.font_name.as_str() {
        "Helvetica" => config.title_font_size * 0.3, // Helvetica is narrower
        _ => config.title_font_size * 0.4,           // For other fonts
    };
    let approx_title_width = config.title.len() as f32 * approx_width_per_char;
    let title_x = x_center - (approx_title_width / 2.0);
    
    // Add title
    operations.extend(vec![
        Operation::new("BT", vec![]), // Begin text
        Operation::new("Tf", vec!["F1".into(), config.title_font_size.into()]), // Set font
        // Position at calculated center position
        Operation::new("Td", vec![title_x.into(), 700.into()]), 
        Operation::new("Tj", vec![Object::string_literal(config.title)]), // Draw text
        Operation::new("ET", vec![]), // End text
    ]);

    // Add each section
    let mut y_position = 650.0;
    for section in config.sections {
        // Add heading
        operations.extend(vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), config.heading_font_size.into()]),
            Operation::new("Td", vec![100.into(), y_position.into()]),
            Operation::new("Tj", vec![Object::string_literal(section.heading)]),
            Operation::new("ET", vec![]),
        ]);

        y_position -= 20.0;

        // Add content
        operations.extend(vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), config.body_font_size.into()]),
            Operation::new("Td", vec![100.into(), y_position.into()]),
            Operation::new("Tj", vec![Object::string_literal(section.content)]),
            Operation::new("ET", vec![]),
        ]);

        y_position -= 30.0;
    }

    let content = Content { operations };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    };

    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });

    doc.trailer.set("Root", catalog_id);
    // Don't compress for test PDFs to avoid potential issues
    // doc.compress();

    doc.save(&config.output_path).unwrap();

    Ok(())
}

pub fn create_test_pdf() -> Result<(), std::io::Error> {
    create_test_pdf_with_config(PdfConfig::default())
}

#[test]
fn test_create_test_pdf() {
    assert!(create_test_pdf().is_ok());
}

#[test]
fn test_create_custom_pdf() {
    let config = PdfConfig {
        title: "Custom PDF".to_string(),
        sections: vec![Section {
            heading: "Test Section".to_string(),
            content: "Test Content".to_string(),
        }],
        font_name: "Helvetica".to_string(),
        title_font_size: 36.0,
        heading_font_size: 18.0,
        body_font_size: 10.0,
        output_path: "tests/custom.pdf".to_string(),
    };

    assert!(create_test_pdf_with_config(config).is_ok());
}
