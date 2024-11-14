use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;

use std::path::PathBuf;

use clap::Parser;

use lopdf::{Dictionary, Document, Encoding, Error as LopdfError, Object, Result as LopdfResult};
pub mod dom;
pub mod layout;
pub mod parse;
use crate::dom::*;
use crate::layout::*;
use crate::parse::*;

use log::{debug, error, warn};

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

fn main() -> Result<(), lopdf::Error> {
    println!("Starting PDF processing");

    // Read and parse the template file
    let template_str = std::fs::read_to_string("10k.tmpl").expect("Failed to read template file");

    let dom = parse_template(&template_str);
    println!("Parsed template: {:?}", dom);

    let pdf_path = "tests/3M_2015_10k.pdf";
    let doc = Document::load(pdf_path)?;

    // Extract text elements with metadata
    let text_elements = get_pdf_text(&doc)?;

    // Define the search string from your template
    let search_string = "Discussion and Analysis of Financial Condition and Results of Operations";

    // Perform matching
    let matched_elements = perform_matching(text_elements, search_string);

    // Apply heuristics to select the best match
    if let Some(best_match) = select_best_match(matched_elements) {
        println!(
            "Best match found on page {}: {}",
            best_match.page_number, best_match.text
        );
        println!("Font size: {}", best_match.font_size);
        println!("Position: {:?}", best_match.position);

        // Proceed to extract the section content starting from this match
        // You may need to implement additional logic to collect the section content
    } else {
        println!("No matching section found.");
    }

    Ok(())
}
