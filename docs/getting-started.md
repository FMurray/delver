---
layout: page
title: Getting Started
description: "Quick setup guide to get Delver running on your system"
toc: true
---

# Getting Started with Delver

Welcome to Delver! This guide will help you get up and running quickly with document parsing and content extraction.

## Installation

### Prerequisites

- **Rust**: Delver is built in Rust. Install from [rustup.rs](https://rustup.rs/)
- **Python** (optional): For Python bindings, requires Python 3.8+

### From Source

```bash
# Clone the repository
git clone https://github.com/yourusername/delver.git
cd delver

# Build the project
cargo build --release

# Run tests to verify installation
cargo test
```

### Python Bindings

```bash
# Install from PyPI (when available)
pip install delver

# Or build from source
pip install maturin
maturin develop
```

## Basic Usage

### 1. Your First Template

Create a simple template to extract text chunks:

```rust
// simple.tmpl
TextChunk(
  chunkSize=500,
  chunkOverlap=150
)
```

### 2. Process a Document

```bash
# Command line usage
delver --template simple.tmpl --input document.pdf --output chunks.json

# With configuration
delver --config config.toml --template advanced.tmpl --input document.pdf
```

### 3. Python Integration

```python
import delver

# Load and process document
parser = delver.Parser("template.tmpl")
result = parser.process("document.pdf")

# Access extracted chunks
for chunk in result.chunks:
    print(f"Content: {chunk.content}")
    print(f"Metadata: {chunk.metadata}")
```

## Template Examples

### Extract Specific Sections

```rust
Section(match="Executive Summary", as="exec_summary") {
  TextChunk(
    chunkSize=300,
    chunkOverlap=50,
    addMeta=[exec_summary]
  )
}

Section(match="Financial Results", as="financials") {
  Table(model="table_extractor")
  TextChunk(chunkSize=500, chunkOverlap=100)
}
```

### Handle Nested Sections

```rust
Section(match="Management Discussion", as="mgmt") {
  Section(match="Risk Factors", as="risks") {
    TextChunk(
      chunkSize=400,
      chunkOverlap=75,
      addMeta=[mgmt, risks]
    )
  }
  
  Section(match="Market Analysis", as="market") {
    TextChunk(chunkSize=600, chunkOverlap=120)
    Table(model="financial_table_parser")
  }
}
```

### Fuzzy Matching

```rust
Section(
  match="Quaterly Results",  // Note: intentional typo
  threshold=0.8,            // Allow fuzzy matching
  as="quarterly"
) {
  TextChunk(chunkSize=500, chunkOverlap=100)
}
```

## Configuration

Create a `config.toml` file for advanced settings:

```toml
[parsing]
# PDF processing settings
extract_images = true
extract_tables = true
preserve_layout = true

[chunking]
# Default chunk settings
default_chunk_size = 500
default_overlap = 150
token_counting = "tiktoken"  # or "simple"

[matching]
# Fuzzy matching settings
default_threshold = 0.8
algorithm = "levenshtein"

[output]
# Output format settings
format = "json"  # json, xml, csv
include_metadata = true
preserve_hierarchy = true

[models]
# Optional ML model integration
table_parser = "path/to/table/model"
image_captioner = "path/to/caption/model"
```

## Common Patterns

### Document Overview
Start with a template that extracts everything to understand document structure:

```rust
TextChunk(chunkSize=1000, chunkOverlap=200)
```

### Section-by-Section Analysis
Once you understand the structure, target specific sections:

```rust
Section(match="Introduction") {
  TextChunk(chunkSize=300, chunkOverlap=50)
}

Section(match="Methodology") {
  TextChunk(chunkSize=500, chunkOverlap=100)
}

Section(match="Results") {
  Table(model="results_extractor")
  TextChunk(chunkSize=400, chunkOverlap=75)
}
```

### Hierarchical Extraction
For complex documents with nested structure:

```rust
Section(match="Chapter 1", as="ch1") {
  Section(match="1.1", as="ch1_1") {
    TextChunk(addMeta=[ch1, ch1_1])
  }
  Section(match="1.2", as="ch1_2") {
    TextChunk(addMeta=[ch1, ch1_2])
  }
}
```

## Troubleshooting

### Common Issues

**Template Syntax Errors**
- Check bracket matching and parameter syntax
- Verify section match strings exist in document
- Use fuzzy matching for flexible text matching

**No Matches Found**
- Verify section text exists in the document
- Try fuzzy matching with lower threshold
- Check for formatting differences (spaces, punctuation)

**Performance Issues**
- Reduce chunk overlap for faster processing
- Use specific section matching instead of full document parsing
- Consider parallel processing for multiple documents

### Debug Mode

Enable debug output to understand processing:

```bash
delver --debug --template template.tmpl --input document.pdf
```

### Getting Help

- 📖 Check the [Documentation]({{ '/documentation/' | relative_url }})
- 🐛 Report issues on [GitHub](https://github.com/yourusername/delver/issues)
- 💬 Join our community discussions

## Next Steps

Now that you have Delver running:

1. **Explore [Template Syntax]({{ '/docs/template-syntax/' | relative_url }})** - Learn advanced templating features
2. **Read [Parser Details]({{ '/docs/parser/' | relative_url }})** - Understand the PDF processing pipeline
3. **Study [Content Collation]({{ '/docs/collation/' | relative_url }})** - Learn about section matching algorithms
4. **Review [Implementation Plan]({{ '/docs/implementation-plan/' | relative_url }})** - Understand the technical architecture

Happy parsing! 🎉