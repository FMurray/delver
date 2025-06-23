---
layout: default
title: "High-Performance Document Parsing"
description: "A declarative tool for parsing and splitting unstructured documents, with focus on scanned PDF files"
---

<div class="hero">
  <div class="hero-content">
    <h1 class="hero-title">Delver</h1>
    <p class="hero-subtitle">A high-performance, declarative tool for parsing and splitting unstructured documents, with an initial focus on scanned PDF files (without OCR).</p>
    <div class="hero-buttons">
      <a href="{{ '/getting-started/' | relative_url }}" class="btn btn-primary">Get Started</a>
      <a href="{{ '/docs/template-syntax/' | relative_url }}" class="btn btn-secondary">Documentation</a>
      <a href="https://github.com/yourusername/delver" class="btn btn-outline">View on GitHub</a>
    </div>
  </div>
</div>

<section class="features">
  <div class="wrapper">
    <h2>Why Delver?</h2>
    <div class="features-grid">
      <div class="feature">
        <h3>🚀 High Performance</h3>
        <p>Built in Rust for speed and efficiency, suitable for processing large documents with minimal overhead.</p>
      </div>
      <div class="feature">
        <h3>📝 Declarative Templates</h3>
        <p>Define parsing rules using an intuitive templating language inspired by JSX/HTML. No complex regex patterns.</p>
      </div>
      <div class="feature">
        <h3>🎯 Semantic Matching</h3>
        <p>Match document elements based on their semantics with prebuilt patterns like "title element", "table element".</p>
      </div>
      <div class="feature">
        <h3>🔍 Fuzzy Matching</h3>
        <p>Handle text variations and typos with fuzzy matching techniques like Levenshtein distance.</p>
      </div>
      <div class="feature">
        <h3>🐍 Python Integration</h3>
        <p>Seamless Python bindings via PyO3 for easy integration into existing data pipelines.</p>
      </div>
      <div class="feature">
        <h3>🔧 Extensible</h3>
        <p>Optional integration with machine learning models and GPU resources for advanced processing.</p>
      </div>
    </div>
  </div>
</section>

<section class="example">
  <div class="wrapper">
    <h2>Quick Example</h2>
    <p>Extract and chunk content between sections with simple declarative syntax:</p>
    
```rust
Section(match="Management's Discussion and Analysis", as="section1") {
  Section(match="Section 1.1: Risks", as="section1_1") {
    TextChunk(
      chunkSize=500,
      chunkOverlap=150,
      addMeta=[section1, section1_1]
    )
    Table(model="some_vlm")
  }
}
```

  <p>This template will:</p>
  <ul>
    <li>Find the "Management's Discussion and Analysis" section</li>
    <li>Locate the "Section 1.1: Risks" subsection</li>
    <li>Extract text chunks with specified size and overlap</li>
    <li>Process any tables with a vision-language model</li>
    <li>Attach hierarchical metadata to all extracted content</li>
  </ul>
  </div>
</section>

<section class="getting-started">
  <div class="wrapper">
    <h2>Ready to Get Started?</h2>
    <p>Delver makes document parsing simple and powerful. Choose your path:</p>
    <div class="cta-grid">
      <a href="{{ '/getting-started/' | relative_url }}" class="cta-card">
        <h3>🚀 Quick Start</h3>
        <p>Get up and running with Delver in minutes</p>
      </a>
      <a href="{{ '/docs/template-syntax/' | relative_url }}" class="cta-card">
        <h3>📚 Learn the Syntax</h3>
        <p>Master the declarative template language</p>
      </a>
      <a href="{{ '/docs/implementation-plan/' | relative_url }}" class="cta-card">
        <h3>🔧 Technical Details</h3>
        <p>Deep dive into architecture and implementation</p>
      </a>
    </div>
  </div>
</section>