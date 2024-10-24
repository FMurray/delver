# Delver
## Table of Contents

1. Introduction
2. Goals and Objectives
3. Functional Requirements
4. Non-Functional Requirements
5. System Architecture
6. Data Models
7. Module Design
8. Technology Stack
9. APIs and Interfaces
10. Testing and Validation
11. Risks and Mitigation
12. Implementation Plan
13. Appendices

## 1. Introduction
Delver is a library designed to parse various document types and intelligently split them into meaningful sections. It aims to facilitate tasks such as extracting structured data (e.g., tables) from unstructured documents like SEC PDF filings. The library leverages the performance of Rust for core parsing functionalities and provides a Python interface for ease of use in data processing frameworks like PySpark and Ray.

## 2. Goals and Objectives
Efficient Parsing: Utilize Rust for fast and memory-efficient parsing of large documents.
Python Integration: Provide a Python API for seamless integration with data science workflows.
Intelligent Sectioning: Implement advanced algorithms and ML models to split documents into logical sections.
Template-Based Extraction: Allow users to define custom extraction rules using a templating language like Jinja.

## 3. Functional Requirements
### 3.1 Document Parsing
- Support Multiple Formats: Ability to parse PDFs, DOCXs, and other common document types.
- Text Extraction: Extract raw text from documents while preserving the structural hierarchy (headings, paragraphs, lists).
- Metadata Extraction: Extract metadata such as authorship, creation date, and document properties.
### 3.2 Intelligent Sectioning
Section Identification: Identify and label sections based on headings and subheadings.

Custom Section Grouping: Allow users to define custom grouping of text based on headings using expressions.

Example:

jinja
{% for paragraph in parsed.sections.get(expr(between("h2:Management's Discussion and Analysis MD&A", next(#h2)))) %}
  {{ paragraph }}
{% endfor %}
Content Between Headings: Extract all content between specified headings.

### 3.3 Table Extraction
- Structured Table Extraction: Use ML models to detect and extract tables, converting them into structured formats (e.g., CSV, DataFrames).
- Table Formatting: Preserve the original formatting and relationships within tables.

### 3.4 API Accessibility
- Python API: Expose parsing functionalities through a Python interface.
- Cluster Compatibility: Ensure the library works efficiently in distributed environments like PySpark and Ray clusters.
### 3.5 Extensibility
- Plugin Support: Allow users to add support for new document types or parsing strategies.
- Template Customization: Enable advanced users to customize extraction templates.

## 4. Non-Functional Requirements
- Performance: Parsing should be fast and have low memory overhead.
- Scalability: Efficiently handle large documents and scale in distributed systems.
- Usability: Provide clear documentation and user-friendly APIs.
- Reliability: Ensure consistent parsing results across different document types.
- Security: Safely handle untrusted documents without exposing vulnerabilities.

## 5. System Architecture
### 5.1 High-Level Overview
The delver library consists of two main components:

- Core Parser (Rust): Handles the heavy lifting of parsing documents and extracting raw data.
- Python Interface: Wraps the Rust core and provides an API for Python users.

### 5.2 Component Interaction
- Input: User provides a document file to the Python API.
- Processing: The Python layer calls the Rust core to parse the document.
- Extraction: Parsed data is processed using templates or expressions.
- Output: Structured data is returned to the user.

## 6. Data Models
### 6.1 Document Object Model (DOM)
Represents the hierarchical structure of the document.
Nodes include elements like headings, paragraphs, lists, and tables.

### 6.2 Section Objects
Attributes: Heading level, title, content, and metadata.
Methods: Functions to access child sections, content extraction, etc.
### 6.3 Table Objects
Represent tables with rows, columns, and cells.
Methods to convert tables into pandas DataFrames or CSV format.

## 7. Module Design
### 7.1 Core Parser Module (Rust)
- File Parsers: Specific parsers for different document types (PDF, DOCX).
- Text Extractors: Extract text while maintaining structure.
- ML Models Integration: Incorporate models for table detection.

### 7.2 Python Interface Module
- Wrapper Classes: Python classes that wrap Rust functionalities.
- Template Engine: Integrate Jinja or a similar templating engine.
- Utilities: Helper functions for common tasks.

### 7.3 Template Processing Module
- Expression Evaluator: Interpret and execute expressions within templates.
- Section Selector: Functions to select and iterate over document sections.

## 8. Technology Stack
- Programming Languages:
  - Rust (Core parsing functionalities)
  - Python (API and user interface)
- Templating Engine:
  - Jinja2 (for template-based extraction)
- ML Frameworks:
  - PyTorch or TensorFlow (for ML models used in table extraction)
- Data Processing Frameworks:
  - Support for PySpark and Ray clusters

## 9. APIs and Interfaces
### 9.1 Python API
parse_document(file_path): Parses a document and returns a Document object.
- Document.sections: Access to the hierarchical sections of the document.
- Document.get_tables(): Extracts tables from the document.
- Section.get_content(): Retrieves the text content of a section.

### 9.2 Template Functions
expr(): Evaluates an expression for section selection.
between(start, end): Selects content between two headings.
next(selector): Selects the next element matching the selector.

### 9.3 CLI (Optional)
Command-line interface for parsing documents and outputting results.

## 10. Testing and Validation
### 10.1 Unit Tests
- For individual functions and methods in both Rust and Python code.

### 10.2 Integration Tests
- Testing the end-to-end parsing and extraction process.
### 10.3 Compatibility Tests
- Ensure the library works across different environments and document types.

## 11. Risks and Mitigation
### 11.1 Complex Documents
- Risk of failing to parse documents with unconventional structures.
- Mitigation: Implement robust parsing strategies and allow for user customization.

### 11.2 ML Model Accuracy
- Extracted tables might not be accurate.
- Mitigation: Continuously train and improve ML models; allow users to provide feedback.

### 11.3 Performance Bottlenecks
- Potential slowdowns when processing very large documents.
- Mitigation: Optimize code and allow for distributed processing.

## 12. Implementation Plan
### Phase 1: Develop the core parsing functionalities in Rust.
### Phase 2: Create the Python interface and ensure seamless integration.
### Phase 3: Implement the templating system with Jinja.
### Phase 4: Integrate ML models for table extraction.
### Phase 5: Test in distributed environments like PySpark and Ray.
### Phase 6: Documentation and user guides.
### Phase 7: Beta release and collect user feedback.

## 13. Appendices
### A. Example Usage

```python from delver import parse_document

doc = parse_document('sec_filing.pdf')

# Using template to extract MD&A section
mdna_content = doc.sections.get(expr(between("h2:Management's Discussion and Analysis MD&A", next("#h2"))))

for paragraph in mdna_content:
    print(paragraph)
```

### B. Expression Language Specification
- Selectors: Patterns to match headings, paragraphs, etc.
- Operators: between, next, prev, logical operators (and, or, not).