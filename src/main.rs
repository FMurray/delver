use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use delver_pdf::persistent_store::PersistentDebugStore;
use delver_pdf::process_pdf;

#[cfg(feature = "debug-viewer")]
use delver_pdf::debug_viewer::launch_async_viewer;

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

    /// Jaeger query URL (default: http://localhost:16686)
    #[clap(long, default_value = "http://localhost:16686")]
    pub jaeger_url: String,

    /// Service name for OpenTelemetry traces (default: delver-pdf)
    #[clap(long, default_value = "delver-pdf")]
    pub service_name: String,

    /// Wait for Jaeger to be ready (timeout in seconds, default: 30)
    #[clap(long, default_value_t = 30)]
    pub jaeger_timeout: u64,
}

impl Args {
    pub fn parse_args() -> Self {
        Args::parse()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse_args();

    // Initialize the persistent debug store
    let debug_store = PersistentDebugStore::new(&args.jaeger_url, &args.service_name)?;

    // Wait for Jaeger to be ready (optional, but helpful for development)
    println!("Waiting for Jaeger to be ready at {}...", args.jaeger_url);
    if let Err(e) = debug_store.wait_for_jaeger(args.jaeger_timeout).await {
        eprintln!("Warning: Could not connect to Jaeger: {}. Proceeding anyway...", e);
        eprintln!("Make sure Jaeger is running. You can start it with:");
        eprintln!("  docker run -d --name jaeger \\");
        eprintln!("    -p 16686:16686 \\");
        eprintln!("    -p 14268:14268 \\");
        eprintln!("    -p 4317:4317 \\");
        eprintln!("    jaegertracing/all-in-one:latest");
    } else {
        println!("Jaeger is ready!");
    }

    // Initialize OpenTelemetry tracing
    if let Err(e) = debug_store.init_tracing().await {
        eprintln!("Warning: Failed to initialize OpenTelemetry: {}. Continuing with limited tracing...", e);
    } else {
        println!("OpenTelemetry tracing initialized successfully");
    }

    // Process PDF
    let pdf_bytes = fs::read(&args.pdf_path)?;
    let template_str = fs::read_to_string(&args.template)?;
    let (json, _blocks, _doc) = process_pdf(&pdf_bytes, &template_str)?;

    println!("PDF processing completed. Traces have been sent to Jaeger.");

    // Launch async debug viewer if enabled
    #[cfg(feature = "debug-viewer")]
    {
        println!("Launching async debug viewer...");
        println!("The viewer will query traces from Jaeger at: {}", args.jaeger_url);
        launch_async_viewer(&_doc, &_blocks, debug_store)?;
    }

    // Output results
    match args.output {
        Some(path) => fs::write(&path, json)?,
        None => println!("{}", json),
    }

    Ok(())
}
