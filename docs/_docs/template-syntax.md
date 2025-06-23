---
layout: doc
title: Template Syntax
description: "Complete guide to Delver's declarative template language for document parsing"
toc: true
tags: [templates, syntax, DSL]
---

# Template Syntax Guide

Delver uses a DOM-like syntax to declaratively define the desired output. If you are familiar with HTML and templating languages like JSX, you will find Delver's syntax familiar and intuitive.

## Philosophy

The core philosophy behind Delver's template syntax is **declarative simplicity**. Instead of writing complex procedural code or regex patterns, you describe *what* you want to extract using a structure that mirrors the document's hierarchy.

## Core Concepts

### Sections and Nodes

The Delver DOM is composed of two primary building blocks:

- **Sections**: Container nodes that define document boundaries and can contain other nodes
- **Nodes**: Leaf elements that represent specific content to extract (text chunks, tables, images)

### The DOM Tree Structure

Elements are organized in a hierarchical tree structure where:
- **Siblings** are elements at the same nesting level
- **Children** are elements nested within a parent
- **Parents** contain and control the scope for their children

## Section Syntax

Sections are the fundamental building blocks for document structure definition:

```rust
Section(
  match="Management's Discussion and Analysis",     // Required: section start pattern
  threshold=0.8,                                   // Optional: fuzzy match threshold (0.0-1.0)
  end_match="Quantitative Disclosures",           // Optional: section end pattern  
  as="mda_section"                                 // Optional: metadata label
) {
  // Child nodes and sections go here
  TextChunk(chunkSize=500, chunkOverlap=150)
}
```

### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `match` | String | Yes | Text pattern to match section start |
| `threshold` | Float | No | Fuzzy matching threshold (default: 0.6) |
| `end_match` | String | No | Text pattern to match section end |
| `as` | String | No | Metadata label for the section |

### Section Matching Behavior

**Sequential Processing**: Sections are processed in the order they appear in the template. Each section extracts content from where the previous section ended until its own end boundary.

**Boundary Detection**: 
- **Start**: Uses the `match` parameter with optional fuzzy matching
- **End**: Uses `end_match` if provided, otherwise continues to the next sibling or document end

## Section Nesting

Sections can be arbitrarily nested to match complex document hierarchies:

```rust
Section(match="Financial Performance", as="financial") {
  TextChunk(chunkSize=300, chunkOverlap=50)
  
  Section(match="Revenue Analysis", as="revenue") {
    Table(model="financial_table_parser")
    TextChunk(chunkSize=400, chunkOverlap=75)
    
    Section(match="Quarterly Breakdown", as="quarterly") {
      TextChunk(
        chunkSize=250,
        chunkOverlap=25,
        addMeta=[financial, revenue, quarterly]
      )
    }
  }
  
  Section(match="Expense Analysis", as="expenses") {
    Table(model="expense_table_parser")
    TextChunk(chunkSize=400, chunkOverlap=75)
  }
}
```

## Node Types

### TextChunk

Extracts and processes text content with configurable chunking:

```rust
TextChunk(
  chunkSize=500,        // Chunk size in tokens
  chunkOverlap=150,     // Overlap between chunks in tokens
  addMeta=[section1]    // Metadata to attach to each chunk
)
```

#### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `chunkSize` | Integer | Yes | Size of each text chunk in tokens |
| `chunkOverlap` | Integer | No | Number of overlapping tokens between chunks |
| `addMeta` | Array | No | List of metadata labels to attach |

### Table

Processes table content, optionally with ML models:

```rust
Table(
  model="table_extraction_model",    // Optional: ML model for processing
  chunkSize=500,                     // Optional: chunk size for table text
  chunkOverlap=100                   // Optional: chunk overlap for table text
)
```

### Image

Extracts and processes image content:

```rust
Image(
  model="image_captioning_model",    // Optional: ML model for image processing
  extract_text=true                 // Optional: extract text from images
)
```

## Advanced Features

### Fuzzy Matching

Handle text variations, typos, and formatting inconsistencies:

```rust
Section(
  match="Executive Sumary",  // Note: intentional typo
  threshold=0.85,           // Allow 85% similarity
  as="exec_summary"
) {
  TextChunk(chunkSize=400, chunkOverlap=50)
}
```

**Threshold Guidelines**:
- `0.9-1.0`: Very strict matching (minor typos only)
- `0.8-0.9`: Moderate flexibility (typos and formatting)
- `0.6-0.8`: High flexibility (significant variations)
- `<0.6`: Very loose matching (use with caution)

### Metadata Inheritance

Metadata flows down the hierarchy automatically:

```rust
Section(match="Financial Results", as="financials") {
  Section(match="Q1 Results", as="q1") {
    Section(match="Revenue", as="revenue") {
      TextChunk(
        chunkSize=300,
        chunkOverlap=50,
        addMeta=[detailed_analysis]  // Final metadata: [financials, q1, revenue, detailed_analysis]
      )
    }
  }
}
```

### Conditional Processing

Use different processing strategies based on content type:

```rust
Section(match="Data Analysis", as="analysis") {
  // Extract both tables and text
  Table(model="data_table_parser")
  TextChunk(chunkSize=500, chunkOverlap=100)
  
  // Process any images in the section
  Image(model="chart_analyzer")
}
```

## Complete Examples

### Basic Document Processing

```rust
// Extract all content with consistent chunking
TextChunk(
  chunkSize=500,
  chunkOverlap=150
)
```

### Structured Report Processing

```rust
Section(match="Executive Summary", as="summary") {
  TextChunk(chunkSize=300, chunkOverlap=50)
}

Section(match="Introduction", as="intro") {
  TextChunk(chunkSize=400, chunkOverlap=75)
}

Section(match="Methodology", as="methods") {
  TextChunk(chunkSize=500, chunkOverlap=100)
  Table(model="methodology_table_parser")
}

Section(match="Results", as="results") {
  Table(model="results_table_parser")
  Image(model="chart_analyzer")
  TextChunk(chunkSize=400, chunkOverlap=75)
}

Section(match="Conclusion", as="conclusion") {
  TextChunk(chunkSize=300, chunkOverlap=50)
}
```

### Financial Document Processing

```rust
Section(match="Management's Discussion and Analysis", as="mda") {
  Section(match="Overview", as="overview") {
    TextChunk(
      chunkSize=400,
      chunkOverlap=75,
      addMeta=[management_overview]
    )
  }
  
  Section(match="Financial Condition", as="financial_condition") {
    Table(model="financial_table_extractor")
    TextChunk(chunkSize=500, chunkOverlap=100)
    
    Section(match="Liquidity", as="liquidity") {
      TextChunk(chunkSize=300, chunkOverlap=50)
    }
    
    Section(match="Capital Resources", as="capital") {
      Table(model="capital_table_parser")
      TextChunk(chunkSize=350, chunkOverlap=60)
    }
  }
  
  Section(match="Results of Operations", as="operations") {
    Table(model="operations_table_parser")
    TextChunk(chunkSize=450, chunkOverlap=90)
  }
}

Section(match="Risk Factors", as="risks") {
  TextChunk(
    chunkSize=400,
    chunkOverlap=75,
    addMeta=[risk_analysis]
  )
}
```

## Best Practices

### Template Design

1. **Start Simple**: Begin with basic text extraction and add complexity gradually
2. **Test Incrementally**: Verify each section matches correctly before adding nested structures
3. **Use Descriptive Labels**: Choose meaningful `as` values for better metadata organization
4. **Consider Document Structure**: Mirror the document's natural hierarchy in your template

### Matching Strategies

1. **Exact Matching First**: Start with exact text matches when possible
2. **Gradual Fuzzy Matching**: Increase threshold gradually if exact matching fails
3. **Validate Boundaries**: Ensure section boundaries make logical sense
4. **Handle Edge Cases**: Account for formatting variations and optional sections

### Performance Optimization

1. **Appropriate Chunk Sizes**: Balance between context preservation and processing efficiency
2. **Minimal Overlap**: Use only necessary overlap to maintain context
3. **Targeted Processing**: Use specific sections rather than processing entire documents
4. **Model Selection**: Choose appropriate ML models for content types

## Troubleshooting

### Common Issues

**No Matches Found**
```rust
// Problem: Exact match fails due to formatting
Section(match="EXECUTIVE SUMMARY") { ... }

// Solution: Use fuzzy matching
Section(match="Executive Summary", threshold=0.8) { ... }
```

**Overlapping Sections**
```rust
// Problem: Sections might overlap unintentionally
Section(match="Section A") { ... }
Section(match="Section A.1") { ... }  // This might be inside Section A

// Solution: Use proper nesting
Section(match="Section A") {
  Section(match="Section A.1") { ... }
}
```

**Missing Content**
```rust
// Problem: No leading TextChunk means content before first section is lost
Section(match="Chapter 1") { ... }

// Solution: Add leading TextChunk for preliminary content
TextChunk(chunkSize=500, chunkOverlap=150)
Section(match="Chapter 1") { ... }
```

### Debug Techniques

1. **Start with Full Extraction**: Use a simple `TextChunk` to see all available content
2. **Test Section Matching**: Use debug mode to verify section boundaries
3. **Validate Hierarchy**: Ensure nested sections are properly contained
4. **Check Metadata Flow**: Verify metadata inheritance works as expected

## What's Next?

- Learn about [Content Collation]({{ '/docs/collation/' | relative_url }}) algorithms
- Understand [Parser Details]({{ '/docs/parser/' | relative_url }}) for PDF processing
- Explore the [Implementation Plan]({{ '/docs/implementation-plan/' | relative_url }}) for technical architecture