use lopdf::content::{Content, Operation};
use lopdf::dictionary;
use lopdf::{Document, Object, Stream};

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

    // Add title
    operations.extend(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), config.title_font_size.into()]),
        Operation::new("Td", vec![100.into(), 600.into()]),
        Operation::new("Tj", vec![Object::string_literal(config.title)]),
        Operation::new("ET", vec![]),
    ]);

    // Add each section
    let mut y_position = 550.0;
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
    });

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };

    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });

    doc.trailer.set("Root", catalog_id);
    doc.compress();

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
