use std::fs;
use std::path::PathBuf;

use clap::Parser;

use delver_pdf::process_pdf;

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse_args();

    // Initialize logging with optional custom directory and keep the guard alive
    let _guard = if let Some(log_dir) = args.log_dir {
        delver_pdf::logging::init_logging_with_dir(args.debug_ops, log_dir)
    } else {
        delver_pdf::logging::init_logging(args.debug_ops)
    };

    // Read the PDF file
    let pdf_bytes = fs::read(&args.pdf_path)?;

    // Read the template file
    let template_str = fs::read_to_string(&args.template)?;

    // Process the PDF
    let json = process_pdf(&pdf_bytes, &template_str)?;

    // // Output the results
    // match args.output {
    //     Some(path) => {
    //         fs::write(&path, json)?;
    //         info!("Output written to: {:?}", path);
    //     }
    //     None => println!("{}", json),
    // }

    Ok(())
}
