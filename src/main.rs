use std::fs;
use std::path::PathBuf;

use clap::Parser;
use anyhow::Result;

use delver_pdf::logging::{init_debug_logging, DebugDataStore};
use delver_pdf::process_pdf;

#[cfg(feature = "debug-viewer")]
use delver_pdf::debug_viewer::launch_viewer;

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
    let (json, blocks, doc) = process_pdf(&pdf_bytes, &template_str)?;

    #[cfg(feature = "debug-viewer")]
    launch_viewer(&doc, &blocks, debug_store)?;

    match args.output {
        Some(path) => fs::write(&path, json)?,
        None => println!("{}", json),
    }
    Ok(())
}
