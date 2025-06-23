---
layout: page
title: Documentation
description: "Complete documentation for Delver's features, APIs, and implementation"
---

# Delver Documentation

Welcome to the comprehensive documentation for Delver, the high-performance document parsing and processing system. This documentation covers everything from basic usage to advanced implementation details.

## Getting Started

<div class="features-grid">
  <div class="feature">
    <h3>🚀 Quick Start</h3>
    <p>Get up and running with Delver in minutes. Learn the basics of installation, configuration, and your first document processing pipeline.</p>
    <a href="{{ '/getting-started/' | relative_url }}" class="btn btn-primary mt-3">Start Here</a>
  </div>
  
  <div class="feature">
    <h3>📝 Template Syntax</h3>
    <p>Master Delver's declarative template language. Learn how to define sections, configure matching, and structure your document parsing rules.</p>
    <a href="{{ '/docs/template-syntax/' | relative_url }}" class="btn btn-secondary mt-3">Learn Syntax</a>
  </div>
</div>

## Technical Documentation

<div class="features-grid">
  <div class="feature">
    <h3>🔍 Parser Details</h3>
    <p>Deep dive into the PDF text extraction parser, transformation pipeline, and coordinate systems. Understand the mathematical foundations.</p>
    <a href="{{ '/docs/parser/' | relative_url }}" class="btn btn-secondary mt-3">Parser Docs</a>
  </div>
  
  <div class="feature">
    <h3>🎯 Content Collation</h3>
    <p>Learn about the sophisticated algorithms that align document content with template structures, including indexing and matching systems.</p>
    <a href="{{ '/docs/collation/' | relative_url }}" class="btn btn-secondary mt-3">Collation Guide</a>
  </div>
  
  <div class="feature">
    <h3>🏗️ Implementation Plan</h3>
    <p>Complete technical roadmap covering system architecture, module design, and development strategy for the entire Delver platform.</p>
    <a href="{{ '/docs/implementation-plan/' | relative_url }}" class="btn btn-secondary mt-3">Architecture</a>
  </div>
  
  <div class="feature">
    <h3>🔧 API Reference</h3>
    <p>Comprehensive API documentation for both Rust and Python interfaces. Includes examples, parameters, and return types.</p>
    <a href="{{ '/api/' | relative_url }}" class="btn btn-secondary mt-3">API Docs</a>
  </div>
</div>

## Key Features

### Declarative Templates
Define document parsing rules using an intuitive, JSX-inspired syntax that describes the structure you want to extract:

```rust
Section(match="Financial Results", as="financials") {
  Table(model="financial_extractor")
  TextChunk(chunkSize=500, chunkOverlap=100)
  
  Section(match="Quarterly Analysis", as="quarterly") {
    TextChunk(chunkSize=300, chunkOverlap=50)
  }
}
```

### High-Performance Processing
- **Rust Core**: Maximum performance for large document processing
- **Multi-Index System**: Efficient content queries using spatial, font, and reference indices
- **Parallel Processing**: Multi-threaded document analysis and extraction
- **Memory Efficient**: Streaming processing for large documents

### Advanced Matching
- **Fuzzy Text Matching**: Handle typos and formatting variations with Levenshtein distance
- **Semantic Matching**: Optional ML-powered content understanding
- **Multi-Criteria Scoring**: Combine text, typography, and spatial cues for accurate section detection
- **Hierarchical Structure**: Respect document organization and nested sections

### Python Integration
Seamless Python bindings for easy integration into existing data pipelines:

```python
import delver

# Load template and process document
parser = delver.Parser("financial_template.tmpl")
result = parser.process("annual_report.pdf")

# Access structured results
for section in result.sections:
    print(f"Section: {section.name}")
    for chunk in section.chunks:
        print(f"  Content: {chunk.text[:100]}...")
        print(f"  Metadata: {chunk.metadata}")
```

## Document Categories

### Financial Documents
- SEC filings (10-K, 10-Q, 8-K)
- Annual and quarterly reports
- Earnings releases
- Regulatory submissions

### Research Papers
- Academic publications
- Technical reports
- White papers
- Conference proceedings

### Legal Documents
- Contracts and agreements
- Court filings
- Legal briefs
- Regulatory documents

### Technical Manuals
- Software documentation
- Product specifications
- User guides
- API documentation

## Best Practices

### Template Design
1. **Start Simple**: Begin with basic text extraction before adding complexity
2. **Test Incrementally**: Validate each section boundary before nesting
3. **Use Descriptive Labels**: Choose meaningful `as` values for metadata
4. **Consider Document Flow**: Mirror the natural document hierarchy

### Performance Optimization
1. **Appropriate Chunking**: Balance context preservation with processing speed
2. **Targeted Extraction**: Use specific section matching instead of full document processing
3. **Batch Processing**: Process multiple similar documents with the same template
4. **Model Selection**: Choose appropriate ML models for your content types

### Error Handling
1. **Graceful Degradation**: Design templates that handle missing sections
2. **Fuzzy Matching**: Use appropriate thresholds for text variations
3. **Validation**: Verify template matches against known good documents
4. **Debugging**: Use debug mode to understand matching behavior

## Community and Support

### Getting Help
- 🐛 **Issues**: Report bugs and request features on [GitHub](https://github.com/yourusername/delver/issues)
- 💬 **Discussions**: Join community discussions for questions and tips
- 📖 **Documentation**: Comprehensive guides and API reference
- 🔄 **Examples**: Real-world templates and use cases

### Contributing
We welcome contributions! Areas where you can help:
- **Templates**: Share templates for common document types
- **Performance**: Optimize core algorithms and data structures
- **Features**: Add new content extractors and ML integrations
- **Documentation**: Improve guides and add examples
- **Testing**: Add test cases for edge cases and document types

### Roadmap
- **Enhanced ML Integration**: Better semantic understanding and structure detection
- **GUI Template Builder**: Visual interface for creating and testing templates
- **Cloud Processing**: Scalable document processing in the cloud
- **Additional Formats**: Support for DOCX, HTML, and other document types
- **Advanced Analytics**: Document analysis and insights beyond extraction

---

Ready to dive deeper? Choose a documentation section above or start with our [Getting Started Guide]({{ '/getting-started/' | relative_url }}) to begin your journey with Delver.