#[cfg(feature = "extension-module")]
use pyo3::prelude::*;
use tokenizers::Tokenizer;

/// Process a PDF file using a template and return extracted data as JSON
#[cfg(feature = "extension-module")]
#[pyfunction]
fn process_pdf_file(pdf_path: String, template_path: String) -> PyResult<String> {
    let pdf_bytes = std::fs::read(pdf_path)?;
    let template_str = std::fs::read_to_string(template_path)?;
    let tokenizer =
        Tokenizer::from_pretrained("Qwen/Qwen2-7B-Instruct", None).unwrap_or_else(|e| {
            panic!("Failed to load tokenizer: {}", e);
        });

    let (json, _blocks, _doc) = delver_core::process_pdf(&pdf_bytes, &template_str, &tokenizer)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

    Ok(json)
}

/// A Python module implemented in Rust
#[cfg(feature = "extension-module")]
#[pymodule(name = "delver_pdf")]
fn delver_pdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(process_pdf_file, m)?)?;
    Ok(())
}
