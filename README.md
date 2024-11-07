# Delver

A high-performance, declarative tool for parsing and splitting unstructured documents, with an initial focus on scanned PDF files (without OCR). This tool allows users to define custom parsing logic using a simple templating system, processing raw file bytes to produce structured outputs.

---

## Table of Contents

- [Introduction](#introduction)
- [Motivation](#motivation)
- [Goals](#goals)
- [Features](#features)
- [Usage](#usage)
  - [Template Syntax](#template-syntax)
  - [Examples](#examples)
- [Technical Details](#technical-details)
  - [Architecture Overview](#architecture-overview)
  - [Key Components](#key-components)
- [Technical Choices](#technical-choices)
- [Dependencies](#dependencies)
- [Future Enhancements](#future-enhancements)
- [Contributing](#contributing)
- [License](#license)

---

## Introduction

Processing unstructured data, especially from scanned PDFs, poses significant challenges due to the lack of inherent structure and metadata. Traditional methods like regex-based parsing are often insufficient for complex documents. Delver aims to provide a robust and flexible solution for parsing and splitting such documents, focusing on semantic elements and leveraging modern technologies for performance and extensibility.

## Motivation

- **Complexity of Unstructured Data**: Handling unstructured documents requires more than simple pattern matching.
- **Need for Performance**: Processing large volumes of data necessitates a high-performance solution.
- **Flexibility**: Users require a tool that can be customized to their specific parsing needs.
- **Semantic Understanding**: Focusing on the semantics of document elements can greatly improve parsing accuracy.

## Goals

- Provide a declarative way to parse and split unstructured documents.
- Implement a robust templating system that is easier and more powerful than regex.
- Focus on semantic matching with prebuilt patterns like "title element", "table element", "image caption", etc.
- Support fuzzy matching techniques like Levenshtein distance.
- Ensure high performance through efficient implementation in Rust.
- Offer optional integration with local machine learning models and GPU resources.

## Features

- **Declarative Templating**: Define parsing rules using an intuitive and expressive templating language.
- **Semantic Matching**: Match document elements based on their semantics rather than just text patterns.
- **Fuzzy Matching**: Utilize fuzzy matching techniques for more flexible pattern recognition.
- **High Performance**: Built in Rust for speed and efficiency, suitable for processing large documents.
- **Extensible Architecture**: Supports integration with machine learning models as optional extras.
- **Python Bindings**: Accessible from Python via PyO3 bindings for easy integration into existing workflows.

## Usage

The tool uses a declarative template to define how the document should be parsed and split.

### Template Syntax

The template uses a simple and expressive syntax where each rule is enclosed in `{}` and parameters are separated by `|`.

#### Parameters

- `match`: Defines what to match in the document.
- `as`: Assigns a label to the matched content for metadata.
- `chunk_size`: Specifies the size of each text chunk in tokens.
- `chunk_overlap`: Specifies the number of overlapping tokens between chunks.
- `add_meta`: Adds metadata to each chunk.
- `model`: Specifies a machine learning model to process the matched content.
- `fuzziness`: (Optional) Sets the Levenshtein distance for fuzzy matching.

### Examples

#### Example 1: Splitting Text Between Headings

```plaintext
Section(match="Section 1: Management Discussion & Analysis", as="section1") {
  Section(match="Section 1.1: Risks", as="section1_1") {
    Section(match="Section 1.1b: Fiscal Risks", as="section1_1b") {
      TextChunk(
        chunkSize=500,
        chunkOverlap=150,
        addMeta=[section1, section1_1, section1_1b]
      )
    }
  }
}
```

This template will:

- Identify the section starting with "About Me" and label it as `mysection`.
- Split all the text between the "About Me" heading and the "My Projects" heading into chunks of 500 tokens, overlapping by 150 tokens.
- Add the `mysection` metadata to each chunk.

#### Example 2: Processing Tables with a Model

```plaintext
{ match: table | model: some_vlm }
```

This template will pass all table elements to a specified machine learning model (`some_vlm`).

#### Example 3: Fuzzy Matching

```plaintext
{ match: # Introduction | fuzziness: 2 | as: intro_section }
```
support nesting
{ match: # {
    { match: ## {
        
    }}
} }

This will match headings similar to "Introduction" within a Levenshtein distance of 2, accounting for minor typos or variations.

## Technical Details

### Architecture Overview

The system consists of several key components, each responsible for a specific part of the parsing and splitting process.

### Key Components

#### Template Parser

- **Function**: Parses the user-defined templates into executable parsing instructions.
- **Implementation**: Uses Rust parser combinator crates like `Nom` or `winnow` for efficient parsing.

#### Document Processor

- **Function**: Processes the document according to the parsing instructions, extracting and transforming content.
- **Implementation**: Utilizes `lopdf` for low-level PDF parsing and manipulation.

#### Semantic Matcher

- **Function**: Identifies document elements based on semantic patterns (e.g., headings, tables).
- **Implementation**: Analyzes document structure and metadata.

#### Fuzzy Matcher

- **Function**: Performs approximate string matching to handle text variations and typos.
- **Implementation**: Uses algorithms like Levenshtein distance.

#### Tokenization Module

- **Function**: Tokenizes text content for chunking operations.
- **Implementation**: Integrates with the `tokenizers` Rust crate for efficient tokenization.

#### Machine Learning Integration

- **Function**: Processes matched content using specified machine learning models.
- **Implementation**: Provides interfaces for optional model invocation, keeping dependencies modular.

#### Python Bindings

- **Function**: Exposes core functionalities to Python applications.
- **Implementation**: Uses `PyO3` to generate Python bindings.

## Technical Choices

- **Language**: Rust for core implementation to ensure performance and safety.
- **Template Parsing**: Parser combinator crates (`Nom` or `winnow`) for flexible and efficient DSL parsing.
- **PDF Manipulation**: `lopdf` crate for low-level PDF access.
- **Tokenization**: `tokenizers` crate for efficient and customizable tokenization.
- **Fuzzy Matching**: Implementing Levenshtein distance algorithms for approximate matching.
- **Python Bindings**: `PyO3` to facilitate integration with Python ecosystems.
- **Modularity**: Optional dependencies for machine learning models and GPU resources.

## Dependencies

- **Rust Crates**:
  - `lopdf` for PDF manipulation.
  - `tokenizers` for text tokenization.
  - `Nom` or `winnow` for parsing the template DSL.
  - `PyO3` for Python bindings.
- **Optional**:
  - Machine learning models (e.g., vision-language models).
  - GPU libraries for hardware acceleration.

## Future Enhancements

- **OCR Support**: Incorporate OCR capabilities to extract text from scanned images.
- **Enhanced DSL**: Develop a more powerful domain-specific language or consider adopting an existing one like JSX.
- **GUI Development**: Create a user-friendly graphical interface for defining templates.
- **Support for More Formats**: Extend support to additional document formats (e.g., DOCX, HTML).
- **Cloud Integration**: Offer cloud-based processing options for scalability.
- **Advanced NLP Features**: Integrate natural language processing techniques for better semantic understanding.

## Contributing

Contributions are welcome! Please feel free to submit issues, fork the repository, and open pull requests.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

*Note: This README.md serves as both a design document and a product requirements document (PRD) for the Unstructured Data Splitter tool. It outlines the project's goals, features, technical implementation, and future plans, providing a comprehensive overview for developers and users alike.*