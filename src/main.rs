use std::collections::HashMap;
use std::fmt::Debug;
use std::path::PathBuf;

use clap::Parser;
use lopdf::Document;
pub mod chunker;
pub mod dom;
pub mod layout;
pub mod parse;
use crate::dom::process_template_element;
use crate::dom::*;
use crate::parse::get_pdf_text;

use serde_json;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about,
    long_about = "Extract TOC and write to file.",
    arg_required_else_help = true
)]
pub struct Args {
    pub pdf_path: PathBuf,

    /// Optional output directory. If omitted the directory of the PDF file will be used.
    #[clap(short, long)]
    pub output: Option<PathBuf>,

    /// Optional pretty print output.
    #[clap(short, long)]
    pub pretty: bool,

    /// Optional password for encrypted PDFs
    #[clap(long, default_value_t = String::from(""))]
    pub password: String,
}

impl Args {
    pub fn parse_args() -> Self {
        Args::parse()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting PDF processing");

    let template_str = std::fs::read_to_string("10k.tmpl")?;
    let dom = parse_template(&template_str)?;
    let pdf_path = "tests/3M_2015_10k.pdf";
    let doc = Document::load(pdf_path)?;
    let text_elements = get_pdf_text(&doc)?;

    let mut all_chunks = Vec::new();

    // Process the template DOM recursively and collect chunks
    for element in dom.elements {
        let mut metadata = HashMap::new();
        all_chunks.extend(process_template_element(
            &element,
            &text_elements,
            &doc,
            &mut metadata,
        ));
    }

    // Write chunks to JSON file
    let json = serde_json::to_string_pretty(&all_chunks)?;
    std::fs::write("chunks.json", json)?;

    Ok(())
}
