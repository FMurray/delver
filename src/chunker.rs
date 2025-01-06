use crate::parse::TextElement;

pub fn chunk_text_elements(
    text_elements: &[TextElement],
    chunk_size: usize,
    chunk_overlap: usize,
) -> Vec<Vec<TextElement>> {
    let mut chunks = Vec::new();
    let mut index = 0;

    while index < text_elements.len() {
        let end = usize::min(index + chunk_size, text_elements.len());
        let chunk = text_elements[index..end].to_vec();
        chunks.push(chunk);

        if end == text_elements.len() {
            break;
        }

        index = end - chunk_overlap;
    }

    chunks
}
