---
layout: page
title: API Reference
description: "Complete API documentation for Delver's Rust and Python interfaces"
---

# API Reference

> 🚧 **Work in Progress**: Comprehensive API documentation is coming soon. This page will be updated as the Delver implementation progresses.

The Delver API provides both Rust-native interfaces and Python bindings for document parsing and content extraction. This reference covers all public APIs, parameters, return types, and usage examples.

## Quick Navigation

- [Rust API](#rust-api)
- [Python Bindings](#python-bindings)
- [Configuration](#configuration)
- [Data Types](#data-types)
- [Error Handling](#error-handling)
- [Examples](#examples)

## Rust API

### Core Parser

```rust
use delver::{Parser, Template, Config};

// Create parser with template
let template = Template::from_file("template.tmpl")?;
let parser = Parser::new(template);

// Process document
let result = parser.process("document.pdf")?;
```

### Template System

```rust
use delver::template::{Template, Section, TextChunk};

// Parse template from string
let template_str = r#"
Section(match="Introduction", as="intro") {
  TextChunk(chunkSize=500, chunkOverlap=100)
}
"#;
let template = Template::from_str(template_str)?;

// Build template programmatically
let template = Template::builder()
    .section("Introduction")
    .as_name("intro")
    .add_child(TextChunk::new(500, 100))
    .build();
```

### Content Extraction

```rust
use delver::extract::{ContentExtractor, MatchConfig};

// Extract with custom matching configuration
let config = MatchConfig::builder()
    .threshold(0.8)
    .algorithm(MatchingAlgorithm::Levenshtein)
    .build();

let extractor = ContentExtractor::with_config(config);
let matches = extractor.find_sections(&document, &template)?;
```

## Python Bindings

### Basic Usage

```python
import delver

# Create parser
parser = delver.Parser("template.tmpl")

# Process document
result = parser.process("document.pdf")

# Access results
for section in result.sections:
    print(f"Section: {section.name}")
    for chunk in section.text_chunks:
        print(f"  Text: {chunk.content}")
        print(f"  Metadata: {chunk.metadata}")
```

### Template Creation

```python
# From file
template = delver.Template.from_file("financial.tmpl")

# From string
template_content = """
Section(match="Executive Summary", as="summary") {
  TextChunk(chunkSize=400, chunkOverlap=75)
}
"""
template = delver.Template.from_string(template_content)

# Programmatic creation
template = (delver.Template.builder()
    .section("Revenue Analysis")
    .fuzzy_threshold(0.85)
    .add_text_chunk(chunk_size=500, overlap=100)
    .build())
```

### Configuration

```python
# Parser configuration
config = delver.Config(
    fuzzy_threshold=0.8,
    parallel_processing=True,
    max_memory_mb=1024,
    debug_mode=False
)

parser = delver.Parser(template, config=config)
```

## Configuration

### Parser Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `fuzzy_threshold` | `f32` | `0.6` | Default threshold for fuzzy text matching |
| `parallel_processing` | `bool` | `true` | Enable multi-threaded processing |
| `max_memory_mb` | `u64` | `512` | Maximum memory usage in MB |
| `debug_mode` | `bool` | `false` | Enable detailed debug output |
| `preserve_layout` | `bool` | `true` | Maintain spatial relationships in output |

### Template Options

| Option | Type | Description |
|--------|------|-------------|
| `match` | `String` | Text pattern to match for section start |
| `threshold` | `f32` | Fuzzy matching threshold (0.0-1.0) |
| `end_match` | `String` | Optional pattern for section end |
| `as` | `String` | Metadata label for the section |
| `chunkSize` | `usize` | Size of text chunks in tokens |
| `chunkOverlap` | `usize` | Overlap between chunks in tokens |
| `model` | `String` | ML model for content processing |

## Data Types

### Core Types

```rust
// Document representation
pub struct Document {
    pub pages: Vec<Page>,
    pub metadata: DocumentMetadata,
}

pub struct Page {
    pub number: u32,
    pub elements: Vec<PageElement>,
    pub dimensions: PageDimensions,
}

// Content elements
pub enum PageElement {
    Text(TextElement),
    Image(ImageElement),
    Table(TableElement),
}

pub struct TextElement {
    pub id: Uuid,
    pub content: String,
    pub bounding_box: Rectangle,
    pub font_info: FontInfo,
    pub page: u32,
}
```

### Extraction Results

```rust
// Extraction results
pub struct ExtractionResult {
    pub sections: Vec<ExtractedSection>,
    pub metadata: ExtractionMetadata,
}

pub struct ExtractedSection {
    pub name: String,
    pub content: SectionContent,
    pub children: Vec<ExtractedSection>,
    pub metadata: HashMap<String, Value>,
}

pub enum SectionContent {
    TextChunks(Vec<TextChunk>),
    Table(TableData),
    Image(ImageData),
    Mixed(Vec<ContentElement>),
}
```

## Error Handling

### Error Types

```rust
#[derive(Debug, Error)]
pub enum DelverError {
    #[error("Template parsing failed: {0}")]
    TemplateParse(String),
    
    #[error("PDF processing error: {0}")]
    PdfProcessing(String),
    
    #[error("Section not found: {pattern}")]
    SectionNotFound { pattern: String },
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

### Python Exceptions

```python
# Delver-specific exceptions
try:
    result = parser.process("document.pdf")
except delver.TemplateError as e:
    print(f"Template error: {e}")
except delver.ProcessingError as e:
    print(f"Processing error: {e}")
except delver.SectionNotFoundError as e:
    print(f"Section not found: {e.pattern}")
```

## Examples

### Financial Document Processing

```rust
use delver::prelude::*;

// Template for 10-K filing
let template = Template::from_str(r#"
Section(match="Management's Discussion and Analysis", as="mda") {
  Section(match="Overview", as="overview") {
    TextChunk(chunkSize=400, chunkOverlap=75)
  }
  
  Section(match="Financial Condition", as="financial") {
    Table(model="financial_table_extractor")
    TextChunk(chunkSize=500, chunkOverlap=100)
  }
  
  Section(match="Results of Operations", as="operations") {
    TextChunk(chunkSize=450, chunkOverlap=90)
  }
}
"#)?;

let parser = Parser::new(template);
let result = parser.process("10k_filing.pdf")?;

// Process results
for section in result.sections {
    println!("Section: {}", section.name);
    
    match section.content {
        SectionContent::TextChunks(chunks) => {
            for chunk in chunks {
                println!("  Chunk: {}", chunk.content);
                println!("  Metadata: {:?}", chunk.metadata);
            }
        },
        SectionContent::Table(table) => {
            println!("  Table with {} rows", table.rows.len());
        },
        _ => {}
    }
}
```

### Research Paper Processing

```python
import delver

# Template for academic paper
template_content = """
Section(match="Abstract", as="abstract") {
  TextChunk(chunkSize=300, chunkOverlap=50)
}

Section(match="Introduction", as="introduction") {
  TextChunk(chunkSize=500, chunkOverlap=100)
}

Section(match="Methodology", as="methods") {
  TextChunk(chunkSize=600, chunkOverlap=120)
  Table(model="research_table_extractor")
}

Section(match="Results", as="results") {
  TextChunk(chunkSize=400, chunkOverlap=75)
  Image(model="chart_analyzer")
}

Section(match="Conclusion", as="conclusion") {
  TextChunk(chunkSize=300, chunkOverlap=60)
}
"""

# Process paper
parser = delver.Parser.from_string(template_content)
result = parser.process("research_paper.pdf")

# Extract structured data
paper_data = {
    "abstract": result.get_section("abstract").text,
    "introduction": result.get_section("introduction").text,
    "methods": {
        "text": result.get_section("methods").text,
        "tables": result.get_section("methods").tables
    },
    "results": {
        "text": result.get_section("results").text,
        "charts": result.get_section("results").images
    },
    "conclusion": result.get_section("conclusion").text
}
```

---

## Coming Soon

The following features are planned for future API releases:

- **Streaming Processing**: Process large documents without loading entirely into memory
- **Batch Operations**: Process multiple documents with the same template
- **Custom Extractors**: Plugin system for custom content extraction logic
- **Advanced ML Integration**: Seamless integration with popular ML frameworks
- **Performance Profiling**: Built-in tools for analyzing processing performance

Stay tuned for updates as the Delver implementation progresses!