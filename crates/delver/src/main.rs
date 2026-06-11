//! `delver` CLI: process PDFs with DocQL templates and manage the persistent
//! document index (docs/DECISIONS.md D-012).
//!
//! Subcommands `index`/`query`/`search` print a single JSON document to
//! stdout; diagnostics go to stderr.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgGroup, Args, Parser, Subcommand};
use uuid::Uuid;

use delver::{
    build_embedder, connect_store, ingest_file, load_tokenizer, run_template_on_doc, search_store,
};
use delver_core::logging::{init_debug_logging, DebugDataStore};
use delver_core::process_pdf;
use delver_store::DocumentId;
use tokenizers::Tokenizer;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about,
    long_about = "Parse, index, query, and search PDF documents with DocQL templates.",
    arg_required_else_help = true
)]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Extract template outputs from a PDF (fresh parse, no database)
    Process(ProcessArgs),
    /// Parse a PDF and persist its element index to Postgres
    Index(IndexArgs),
    /// Execute a DocQL template against a stored document or a PDF file
    Query(QueryArgs),
    /// Full-text search over a stored corpus or document
    Search(SearchArgs),
}

#[derive(Args, Debug)]
struct ProcessArgs {
    /// Path to the PDF file to process
    pdf_path: PathBuf,

    /// Path to the template file
    #[clap(short, long)]
    template: PathBuf,

    /// Optional output file path. If omitted, writes to stdout.
    #[clap(short, long)]
    output: Option<PathBuf>,

    /// Optional pretty print output.
    #[clap(short, long)]
    pretty: bool,

    /// Optional password for encrypted PDFs
    #[clap(long, default_value_t = String::from(""))]
    password: String,

    /// Enable detailed logging of PDF content stream operations
    #[clap(long)]
    debug_ops: bool,

    /// Directory for debug operation logs
    #[clap(long)]
    log_dir: Option<PathBuf>,

    /// Tokenizer model name
    #[clap(long, default_value = "Qwen/Qwen2-7B-Instruct")]
    tokenizer_model: String,

    /// Databricks embedding endpoint (name or full URL) for EmbeddingSim
    /// matches; falls back to $DELVER_EMBED_ENDPOINT
    #[clap(long)]
    embed_endpoint: Option<String>,
}

#[derive(Args, Debug)]
struct IndexArgs {
    /// Path to the PDF file to ingest
    pdf_path: PathBuf,

    /// Corpus name (created if it does not exist)
    #[clap(long)]
    corpus: String,

    /// Optional source URI recorded with the document
    #[clap(long)]
    uri: Option<String>,

    /// Parser version for idempotent re-ingest (D-008)
    #[clap(long, default_value_t = 1)]
    parse_version: i32,

    /// Postgres URL (default: $DATABASE_URL, then the local dev database)
    #[clap(long)]
    db: Option<String>,
}

#[derive(Args, Debug)]
#[clap(group(ArgGroup::new("source").required(true).args(["doc", "pdf"])))]
struct QueryArgs {
    /// Path to the template file
    #[clap(short, long)]
    template: PathBuf,

    /// Stored document id to query (hydrates the index from Postgres)
    #[clap(long)]
    doc: Option<Uuid>,

    /// PDF file to query with a fresh parse (no database)
    #[clap(long)]
    pdf: Option<PathBuf>,

    /// Postgres URL (default: $DATABASE_URL, then the local dev database)
    #[clap(long)]
    db: Option<String>,

    /// Pretty-print the JSON output
    #[clap(short, long)]
    pretty: bool,

    /// Tokenizer model name ("none" for character-based chunking)
    #[clap(long, default_value = "Qwen/Qwen2-7B-Instruct")]
    tokenizer_model: String,

    /// Databricks embedding endpoint (name or full URL) for EmbeddingSim
    /// matches; falls back to $DELVER_EMBED_ENDPOINT
    #[clap(long)]
    embed_endpoint: Option<String>,
}

#[derive(Args, Debug)]
struct SearchArgs {
    /// Full-text query (Postgres plainto_tsquery semantics)
    query: String,

    /// Corpus to search
    #[clap(long)]
    corpus: String,

    /// Restrict the search to one stored document
    #[clap(long)]
    doc: Option<Uuid>,

    /// Maximum number of hits
    #[clap(long, default_value_t = 10)]
    limit: i64,

    /// Postgres URL (default: $DATABASE_URL, then the local dev database)
    #[clap(long)]
    db: Option<String>,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Process(args) => run_process(args),
        Command::Index(args) => run_index(args),
        Command::Query(args) => run_query(args),
        Command::Search(args) => run_search(args),
    }
}

/// Pre-subcommand `delver` behavior, verbatim (D-012).
fn run_process(args: ProcessArgs) -> Result<()> {
    // Initialize debug data store
    let debug_store = DebugDataStore::default();

    // Initialize tracing with debug layer
    let _guard = init_debug_logging(debug_store.clone());

    // Process PDF and launch viewer as before
    let pdf_bytes = fs::read(&args.pdf_path)?;
    let template_str = fs::read_to_string(&args.template)?;
    let tokenizer = Tokenizer::from_pretrained(&args.tokenizer_model, None).ok();
    let embedder = build_embedder(args.embed_endpoint.as_deref())?;
    let (json, _blocks, _doc) = process_pdf(&pdf_bytes, &template_str, tokenizer.as_ref(), embedder)?;

    match args.output {
        Some(path) => fs::write(&path, json)?,
        None => println!("{}", json),
    }
    Ok(())
}

fn run_index(args: IndexArgs) -> Result<()> {
    let store = connect_store(args.db.as_deref())?;
    let value = ingest_file(
        &store,
        &args.pdf_path,
        &args.corpus,
        args.uri.as_deref(),
        args.parse_version,
    )?;
    println!("{value}");
    Ok(())
}

fn run_query(args: QueryArgs) -> Result<()> {
    let template_str = fs::read_to_string(&args.template)?;
    let tokenizer = load_tokenizer(&args.tokenizer_model);
    let embedder = build_embedder(args.embed_endpoint.as_deref())?;

    let json = match (&args.pdf, args.doc) {
        (Some(pdf_path), None) => {
            let pdf_bytes = fs::read(pdf_path)?;
            let (json, _blocks, _doc) =
                process_pdf(&pdf_bytes, &template_str, tokenizer.as_ref(), embedder)?;
            json
        }
        (None, Some(doc)) => {
            let store = connect_store(args.db.as_deref())?;
            run_template_on_doc(
                &store,
                DocumentId(doc),
                &template_str,
                tokenizer.as_ref(),
                embedder,
            )?
        }
        _ => unreachable!("clap group enforces exactly one of --doc / --pdf"),
    };

    if args.pretty {
        println!("{json}");
    } else {
        let value: serde_json::Value = serde_json::from_str(&json)?;
        println!("{value}");
    }
    Ok(())
}

fn run_search(args: SearchArgs) -> Result<()> {
    let store = connect_store(args.db.as_deref())?;
    let value = search_store(
        &store,
        &args.query,
        &args.corpus,
        args.doc.map(DocumentId),
        args.limit,
    )?;
    println!("{value}");
    Ok(())
}
