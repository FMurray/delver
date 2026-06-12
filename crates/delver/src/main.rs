//! `delver` CLI: process PDFs with DocQL templates and manage the persistent
//! document index (docs/DECISIONS.md D-012).
//!
//! Subcommands `index`/`query`/`search` print a single JSON document to
//! stdout; diagnostics go to stderr.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use uuid::Uuid;

use delver::{
    build_embedder, connect_store, infer_partitions_from_path, ingest_file, load_tokenizer,
    parse_key_value, run_template_on_corpus, run_template_on_doc, search_store, IngestEngine,
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

    /// Partition key=value stored with the document (repeatable). Merged
    /// over key=value segments auto-inferred from the input path's
    /// directories (e.g. /loans/state=CA/x.pdf); explicit flags win.
    #[clap(long = "partition", value_name = "KEY=VALUE")]
    partition: Vec<String>,

    /// Parsing engine: native (delver-core, default), ai-parse (Databricks
    /// ai_parse_document; requires DATABRICKS_HOST+DATABRICKS_TOKEN or
    /// DELVER_DBX_PROFILE, plus DELVER_DBX_WAREHOUSE_ID and
    /// DELVER_DBX_VOLUME), or auto (scan classification routes scanned
    /// documents to ai-parse)
    #[clap(long, value_enum, default_value_t = EngineArg::Native)]
    engine: EngineArg,

    /// Postgres URL (default: $DATABASE_URL, then the local dev database)
    #[clap(long)]
    db: Option<String>,
}

/// CLI surface of [`IngestEngine`] (kebab-case values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EngineArg {
    Native,
    AiParse,
    Auto,
}

impl From<EngineArg> for IngestEngine {
    fn from(arg: EngineArg) -> Self {
        match arg {
            EngineArg::Native => IngestEngine::Native,
            EngineArg::AiParse => IngestEngine::AiParse,
            EngineArg::Auto => IngestEngine::Auto,
        }
    }
}

#[derive(Args, Debug)]
#[clap(group(ArgGroup::new("source").required(true).args(["doc", "pdf", "corpus"])))]
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

    /// Run the template across every stored document of this corpus
    /// (filtered by --where); output is keyed by document id
    #[clap(long)]
    corpus: Option<String>,

    /// Partition filter key=value (repeatable; documents must match all).
    /// Only meaningful with --corpus (`requires` would be absorbed by the
    /// source group, so the single-source flags conflict explicitly).
    #[clap(long = "where", value_name = "KEY=VALUE", conflicts_with_all = ["doc", "pdf"])]
    r#where: Vec<String>,

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

    /// Partition filter key=value (repeatable; documents must match all)
    #[clap(long = "where", value_name = "KEY=VALUE", conflicts_with = "doc")]
    r#where: Vec<String>,

    /// Maximum number of hits
    #[clap(long, default_value_t = 10)]
    limit: i64,

    /// Postgres URL (default: $DATABASE_URL, then the local dev database)
    #[clap(long)]
    db: Option<String>,
}

fn main() -> Result<()> {
    // Rust ignores SIGPIPE by default, so `delver query ... | head` would
    // panic with "failed printing to stdout: Broken pipe" when the reader
    // closes the pipe. Restore the conventional Unix behavior (terminate
    // silently on SIGPIPE) before any output happens (D-018).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

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
    // Inferred path partitions first, explicit --partition flags after:
    // partitions_json keeps the last duplicate, so explicit wins (D-023).
    let mut partitions = infer_partitions_from_path(&args.pdf_path);
    for arg in &args.partition {
        partitions.push(parse_key_value(arg)?);
    }
    let value = ingest_file(
        &store,
        &args.pdf_path,
        &args.corpus,
        args.uri.as_deref(),
        args.parse_version,
        &partitions,
        args.engine.into(),
    )?;
    println!("{value}");
    Ok(())
}

fn run_query(args: QueryArgs) -> Result<()> {
    let template_str = fs::read_to_string(&args.template)?;
    let tokenizer = load_tokenizer(&args.tokenizer_model);
    let embedder = build_embedder(args.embed_endpoint.as_deref())?;

    let json = match (&args.pdf, args.doc, &args.corpus) {
        (Some(pdf_path), None, None) => {
            let pdf_bytes = fs::read(pdf_path)?;
            let (json, _blocks, _doc) =
                process_pdf(&pdf_bytes, &template_str, tokenizer.as_ref(), embedder)?;
            json
        }
        (None, Some(doc), None) => {
            let store = connect_store(args.db.as_deref())?;
            run_template_on_doc(
                &store,
                DocumentId(doc),
                &template_str,
                tokenizer.as_ref(),
                embedder,
            )?
        }
        (None, None, Some(corpus)) => {
            let partitions = args
                .r#where
                .iter()
                .map(|arg| parse_key_value(arg))
                .collect::<Result<Vec<_>>>()?;
            let store = connect_store(args.db.as_deref())?;
            run_template_on_corpus(
                &store,
                corpus,
                &partitions,
                &template_str,
                tokenizer.as_ref(),
                embedder,
            )?
        }
        _ => unreachable!("clap group enforces exactly one of --doc / --pdf / --corpus"),
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
    let partitions = args
        .r#where
        .iter()
        .map(|arg| parse_key_value(arg))
        .collect::<Result<Vec<_>>>()?;
    let value = search_store(
        &store,
        &args.query,
        &args.corpus,
        args.doc.map(DocumentId),
        args.limit,
        &partitions,
    )?;
    println!("{value}");
    Ok(())
}
