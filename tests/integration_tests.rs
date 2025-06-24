use anyhow::Result;
use delver_pdf::process_pdf;
use serde_json::Value;
use std::env;
use std::fs;
use std::path::Path;

// TODO: fix this test
#[ignore]
#[test]
fn test_3m_2015_10k() -> Result<()> {
    let workspace_root = env::current_dir()?;
    println!("Running test from workspace root: {:?}", workspace_root);

    let pdf_path = "tests/3M_2015_10K.pdf";
    let template_path = "tests/10k.tmpl";
    let expected_output_path = "tests/3m_2015_10K_1.text.json";

    let pdf_full_path = workspace_root.join(pdf_path);
    println!("Attempting to read PDF from: {:?}", pdf_full_path);
    assert!(
        Path::new(&pdf_full_path).exists(),
        "PDF file not found at {:?}",
        pdf_full_path
    );
    let pdf_bytes = fs::read(&pdf_full_path)?;
    println!("Successfully read PDF file.");

    let template_full_path = workspace_root.join(template_path);
    println!("Attempting to read template from: {:?}", template_full_path);
    assert!(
        Path::new(&template_full_path).exists(),
        "Template file not found at {:?}",
        template_full_path
    );
    let template_str = fs::read_to_string(&template_full_path)?;
    println!("Successfully read template file.");

    let (actual_json_str, _blocks, _doc) = process_pdf(&pdf_bytes, &template_str)?;
    println!("Successfully processed PDF.");

    let expected_output_full_path = workspace_root.join(expected_output_path);
    println!(
        "Attempting to read expected output from: {:?}",
        expected_output_full_path
    );
    assert!(
        Path::new(&expected_output_full_path).exists(),
        "Expected output file not found at {:?}",
        expected_output_full_path
    );
    let expected_json_str = fs::read_to_string(&expected_output_full_path)?;
    println!("Successfully read expected output file.");

    let actual_value: Value = serde_json::from_str(&actual_json_str)?;
    let expected_value: Value = serde_json::from_str(&expected_json_str)?;
    println!("Successfully deserialized JSON outputs.");

    assert_eq!(
        actual_value, expected_value,
        "Processed PDF output does not match expected output."
    );
    println!("JSON outputs match!");

    Ok(())
}
