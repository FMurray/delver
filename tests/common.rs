use std::path::PathBuf;
use std::sync::Once;

static INIT: Once = Once::new();

pub fn setup() {
    INIT.call_once(|| {
        cleanup_all();
    });
}

pub fn cleanup_all() {
    let test_files = [
        "tests/example.pdf",
        "tests/custom.pdf",
        "tests/heading_test.pdf",
    ];

    for file in test_files {
        if std::path::Path::new(file).exists() {
            std::fs::remove_file(file)
                .unwrap_or_else(|e| eprintln!("Failed to remove {}: {}", file, e));
        }
    }
}

pub fn get_test_pdf_path() -> PathBuf {
    PathBuf::from("tests/3M_2015_10K.pdf")
}

pub fn load_test_template() -> String {
    include_str!("../10k.tmpl").to_string()
}
