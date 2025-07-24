use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use delver_core::logging::{init_debug_logging, DebugDataStore};
use delver_core::process_pdf;
use tokenizers::Tokenizer;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about,
    long_about = "Extract TOC and write to file.",
    arg_required_else_help = true
)]
pub struct Args {
    /// Path to the PDF file to process
    pub pdf_path: PathBuf,

    /// Path to the template file
    #[clap(short, long)]
    pub template: PathBuf,

    /// Optional output file path. If omitted, writes to stdout.
    #[clap(short, long)]
    pub output: Option<PathBuf>,

    /// Optional pretty print output.
    #[clap(short, long)]
    pub pretty: bool,

    /// Optional password for encrypted PDFs
    #[clap(long, default_value_t = String::from(""))]
    pub password: String,

    /// Enable detailed logging of PDF content stream operations
    #[clap(long)]
    pub debug_ops: bool,

    /// Directory for debug operation logs
    #[clap(long)]
    pub log_dir: Option<PathBuf>,

    /// Tokenizer model name
    #[clap(long, default_value = "Qwen/Qwen2-7B-Instruct")]
    pub tokenizer_model: String,
}

impl Args {
    pub fn parse_args() -> Self {
        Args::parse()
    }
}

fn main() -> Result<()> {
    let args = Args::parse_args();

    // Initialize debug data store
    let debug_store = DebugDataStore::default();

    // Initialize tracing with debug layer
    let _guard = init_debug_logging(debug_store.clone());

    // Process PDF and launch viewer as before
    let pdf_bytes = fs::read(&args.pdf_path)?;
    let template_str = fs::read_to_string(&args.template)?;
    let tokenizer = Tokenizer::from_pretrained(&args.tokenizer_model, None).unwrap_or_else(|e| {
        panic!("Failed to load tokenizer: {}", e);
    });
    let (json, _blocks, _doc) = process_pdf(&pdf_bytes, &template_str, &tokenizer)?;

    match args.output {
        Some(path) => fs::write(&path, json)?,
        None => println!("{}", json),
    }
    Ok(())
}
