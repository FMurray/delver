use lopdf::content::{Content, Operation};
use lopdf::dictionary;
use lopdf::{Document, Object, Stream};

pub fn create_test_pdf() -> Result<(), std::io::Error> {
    // `with_version` specifes the PDF version this document complies with.
    let mut doc = Document::with_version("1.5");
    // Object IDs are used for cross referencing in PDF documents.
    // `lopdf` helps keep track of them for us. They are simple integers.
    // Calls to `doc.new_object_id` and `doc.add_object` return an object ID.

    // "Pages" is the root node of the page tree.
    let pages_id = doc.new_object_id();

    // Fonts are dictionaries. The "Type", "Subtype" and "BaseFont" tags
    // are straight out of the PDF spec.
    //
    // The dictionary macro is a helper that allows complex
    // key-value relationships to be represented in a simpler
    // visual manner, similar to a match statement.
    // A dictionary is implemented as an IndexMap of Vec<u8>, and Object
    let font_id = doc.add_object(dictionary! {
        // type of dictionary
        "Type" => "Font",
        // type of font, type1 is simple postscript font
        "Subtype" => "Type1",
        // basefont is postscript name of font for type1 font.
        // See PDF reference document for more details
        "BaseFont" => "Courier",
    });

    // Font dictionaries need to be added into resource
    // dictionaries in order to be used.
    // Resource dictionaries can contain more than just fonts,
    // but normally just contains fonts.
    // Only one resource dictionary is allowed per page tree root.
    let resources_id = doc.add_object(dictionary! {
        // Fonts are actually triplely nested dictionaries. Fun!
        "Font" => dictionary! {
            // F1 is the font name used when writing text.
            // It must be unique in the document. It does not
            // have to be F1
            "F1" => font_id,
        },
    });

    // `Content` is a wrapper struct around an operations struct that contains
    // a vector of operations. The operations struct contains a vector of
    // that match up with a particular PDF operator and operands.
    // Refer to the PDF spec for more details on the operators and operands
    // Note, the operators and operands are specified in a reverse order
    // from how they actually appear in the PDF file itself.
    let content = Content {
        operations: vec![
            // BT begins a text element. It takes no operands.
            Operation::new("BT", vec![]),
            // Tf specifies the font and font size.
            // Font scaling is complicated in PDFs.
            // Refer to the spec for more info.
            // The `into()` methods convert the types into
            // an enum that represents the basic object types in PDF documents.
            Operation::new("Tf", vec!["F1".into(), 48.into()]),
            // Td adjusts the translation components of the text matrix.
            // When used for the first time after BT, it sets the initial
            // text position on the page.
            // Note: PDF documents have Y=0 at the bottom. Thus 600 to print text near the top.
            Operation::new("Td", vec![100.into(), 600.into()]),
            // Tj prints a string literal to the page. By default, this is black text that is
            // filled in. There are other operators that can produce various textual effects and
            // colors
            Operation::new("Tj", vec![Object::string_literal("Hello World!")]),
            // ET ends the text element.
            Operation::new("ET", vec![]),
            // First subheading
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]), // Smaller font size
            Operation::new("Td", vec![100.into(), 550.into()]),
            Operation::new("Tj", vec![Object::string_literal("Subheading 1")]),
            Operation::new("ET", vec![]),
            // Text for first section
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]), // Even smaller font size
            Operation::new("Td", vec![100.into(), 530.into()]),
            Operation::new(
                "Tj",
                vec![Object::string_literal("This is the first section text.")],
            ),
            Operation::new("ET", vec![]),
            // Second subheading
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]), // Smaller font size
            Operation::new("Td", vec![100.into(), 500.into()]),
            Operation::new("Tj", vec![Object::string_literal("Subheading 2")]),
            Operation::new("ET", vec![]),
            // Text for second section
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]), // Even smaller font size
            Operation::new("Td", vec![100.into(), 480.into()]),
            Operation::new(
                "Tj",
                vec![Object::string_literal("This is the second section text.")],
            ),
            Operation::new("ET", vec![]),
        ],
    };

    // Streams are a dictionary followed by a (possibly encoded) sequence of bytes.
    // What that sequence of bytes represents, depends on the context.
    // The stream dictionary is set internally by lopdf and normally doesn't
    // need to be manually manipulated. It contains keys such as
    // Length, Filter, DecodeParams, etc.
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));

    // Page is a dictionary that represents one page of a PDF file.
    // Its required fields are "Type", "Parent" and "Contents".
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });

    // Again, "Pages" is the root of the page tree. The ID was already created
    // at the top of the page, since we needed it to assign to the parent element
    // of the page dictionary.
    //
    // These are just the basic requirements for a page tree root object.
    // There are also many additional entries that can be added to the dictionary,
    // if needed. Some of these can also be defined on the page dictionary itself,
    // and not inherited from the page tree root.
    let pages = dictionary! {
        // Type of dictionary
        "Type" => "Pages",
        // Vector of page IDs in document. Normally would contain more than one ID
        // and be produced using a loop of some kind.
        "Kids" => vec![page_id.into()],
        // Page count
        "Count" => 1,
        // ID of resources dictionary, defined earlier
        "Resources" => resources_id,
        // A rectangle that defines the boundaries of the physical or digital media.
        // This is the "page size".
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };

    // Using `insert()` here, instead of `add_object()` since the ID is already known.
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    // Creating document catalog.
    // There are many more entries allowed in the catalog dictionary.
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });

    // The "Root" key in trailer is set to the ID of the document catalog,
    // the remainder of the trailer is set during `doc.save()`.
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    // Store file in current working directory.
    // Note: Line is excluded when running tests
    // if false {
    //     doc.save("example.pdf").unwrap();
    // }
    doc.save("tests/example.pdf").unwrap();

    Ok(())
}

#[test]
fn test_create_test_pdf() {
    // Call the function and assert that it returns Ok
    assert!(create_test_pdf().is_ok());
}
