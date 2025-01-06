# Philosophy

Delver uses a DOM-like syntax to declaratively define the desired output. If you are familiar with HTML and templating languages like JSX, you will find Delver's syntax familiar. 

## Sections and Nodes

Delver DOM is made up of sections and nodes. Sections are the building blocks of the DOM and are the nodes that contain other nodes. The DOM is a tree composed of sections and nodes. 

### Sections

Sections are the nodes that contain other nodes. They are the building blocks of the DOM and are the nodes that contain other nodes. 

### Nodes

Nodes are the "leaf" elements of the DOM. They represent something that you want to extract from the document, such as a table, an image, or a text chunk. 

## Attributes

Attributes are the properties of a node, typically they are used to configure the node's properties, like the chunk size or matching behavior of a section. 

## DOM Layout

DOM elements are siblings if they are on the same nesting level, for example in this template, the first TextChunk and Section are siblings:

```
TextChunk(
    chunkSize=500,
    chunkOverlap=150,
)
Section(match="Management’s Discussion and Analysis of Financial Condition and Results of Operations") {
  TextChunk(
    chunkSize=500,
    chunkOverlap=150,
  )
}
```

The behavior of this template is that first TextChunk will extract all text up to the start of the Section, then the Section will extract all text up to the start of the next sibling, and so on. 

Note that without the first TextChunk, Delver will only extract text beginning with the Section. 

# Section Syntax 

```
Section(
  threshold=0.6, // Optional: matching threshold for the section (uses Levenshtein distance), default is 0.6
  match="Management’s Discussion and Analysis of Financial Condition and Results of Operations", // Required: match the start of the section
  end_match="Quantitative and Qualitative Disclosures About Market Risk", // Optional: match the end of the section, if not provided, the section will be extracted until the end of the document
  as="section1" // Optional: name the section for easier reference
) {
  // Nodes to extract from the section
  TextChunk(
    chunkSize=500,
    chunkOverlap=150,
  )
}
```

## Section Matching

Currently the section matching is done using a simple Levenshtein distance algorithm. This is a good starting point, but it may not be the best for all use cases. Delver will be adding more sophisticated matching algorithms in the future. 

## Section Nesting

Sections can be nested within other sections. This is useful for extracting nested structures within a section, such as tables within a section. Notice that Nodes can be siblings of Sections - for example:

```
Section(match="Management’s Discussion and Analysis of Financial Condition and Results of Operations") {
  TextChunk(
    chunkSize=500,
    chunkOverlap=150,
  )
  Section(match="PERFORMANCE BY BUSINESS SEGMENT") {
    Table(
      model="databricksmodel",
      chunkSize=500,
      chunkOverlap=150,
    )
  }
}
```

# Node Types and Syntax

## TextChunk

TextChunk is a node that extracts text from the document.

```
TextChunk(
  chunkSize=500,
  chunkOverlap=150,
)
```