//! Minimal HTML table → grid parser for ai_parse_document `table` elements
//! (schema 2.0 represents tables "in HTML format"). Deliberately a small,
//! deterministic scanner over the `<table>/<tr>/<td|th>` subset — not a
//! general HTML parser. rowspan/colspan place cells with the standard HTML
//! grid algorithm (spanned slots are occupied; only anchors become cells),
//! matching the `table_cells` addressing where spans live on the anchor row.
//!
//! Fail-loud (D-006): content with no `<tr>` rows or with malformed nesting
//! that yields no cells is an error — never a silently empty table.

use crate::ParseDbxError;

/// One parsed grid cell (anchor position + spans + flattened text).
#[derive(Debug, Clone, PartialEq)]
pub struct HtmlCell {
    pub row: u32,
    pub col: u32,
    pub row_span: u32,
    pub col_span: u32,
    pub text: String,
    pub is_header: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HtmlTable {
    pub n_rows: u32,
    pub n_cols: u32,
    pub cells: Vec<HtmlCell>,
}

/// Parse one HTML table. `<th>` cells and cells inside `<thead>` are headers.
pub fn parse_html_table(html: &str) -> Result<HtmlTable, ParseDbxError> {
    let mut rows: Vec<Vec<(String, String, bool)>> = Vec::new(); // (tag-attrs, text, header)
    let mut current_row: Option<Vec<(String, String, bool)>> = None;
    let mut in_thead = false;

    let mut cursor = 0usize;
    let bytes = html.as_bytes();
    while let Some(open) = html[cursor..].find('<') {
        let tag_start = cursor + open;
        let Some(close) = html[tag_start..].find('>') else {
            break; // unterminated tag: stop scanning, validation below decides
        };
        let tag_end = tag_start + close;
        let tag_body = &html[tag_start + 1..tag_end];
        let tag_lower = tag_body.to_ascii_lowercase();
        let name = tag_lower
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let closing = tag_lower.starts_with('/');

        match (name, closing) {
            ("thead", false) => in_thead = true,
            ("thead", true) => in_thead = false,
            ("tr", false) => {
                if let Some(row) = current_row.take() {
                    rows.push(row); // implicit close of an unclosed <tr>
                }
                current_row = Some(Vec::new());
            }
            ("tr", true) => {
                if let Some(row) = current_row.take() {
                    rows.push(row);
                }
            }
            ("td", false) | ("th", false) => {
                // Cell content runs to the matching close tag (or the next
                // cell/row boundary for sloppy HTML).
                let content_start = tag_end + 1;
                let rest = &html[content_start..];
                let content_end = ["</td", "</th", "<td", "<th", "</tr", "<tr"]
                    .iter()
                    .filter_map(|stop| rest.to_ascii_lowercase().find(stop))
                    .min()
                    .unwrap_or(rest.len());
                let raw = &rest[..content_end];
                if let Some(row) = current_row.as_mut() {
                    row.push((
                        tag_lower.clone(),
                        flatten_text(raw),
                        name == "th" || in_thead,
                    ));
                }
                cursor = content_start + content_end;
                continue;
            }
            _ => {}
        }
        cursor = tag_end + 1;
        if cursor >= bytes.len() {
            break;
        }
    }
    if let Some(row) = current_row.take() {
        rows.push(row);
    }

    if rows.is_empty() {
        return Err(ParseDbxError(format!(
            "table content has no <tr> rows; cannot extract structure from: {}",
            truncate(html, 200)
        )));
    }

    // Standard HTML grid placement with span occupancy.
    let mut occupied: Vec<Vec<bool>> = Vec::new(); // [row][col]
    let mut cells: Vec<HtmlCell> = Vec::new();
    let mut n_cols = 0u32;

    for (row_idx, row) in rows.iter().enumerate() {
        if occupied.len() <= row_idx {
            occupied.resize_with(row_idx + 1, Vec::new);
        }
        let mut col_idx = 0usize;
        for (attrs, text, is_header) in row {
            // Skip slots occupied by spans from earlier rows.
            while occupied
                .get(row_idx)
                .and_then(|r| r.get(col_idx))
                .copied()
                .unwrap_or(false)
            {
                col_idx += 1;
            }
            let row_span = attr_u32(attrs, "rowspan").unwrap_or(1).max(1);
            let col_span = attr_u32(attrs, "colspan").unwrap_or(1).max(1);
            for r in row_idx..row_idx + row_span as usize {
                if occupied.len() <= r {
                    occupied.resize_with(r + 1, Vec::new);
                }
                let row_occ = &mut occupied[r];
                if row_occ.len() < col_idx + col_span as usize {
                    row_occ.resize(col_idx + col_span as usize, false);
                }
                for slot in &mut row_occ[col_idx..col_idx + col_span as usize] {
                    *slot = true;
                }
            }
            cells.push(HtmlCell {
                row: row_idx as u32,
                col: col_idx as u32,
                row_span,
                col_span,
                text: text.clone(),
                is_header: *is_header,
            });
            n_cols = n_cols.max((col_idx as u32) + col_span);
            col_idx += col_span as usize;
        }
    }

    if cells.is_empty() {
        return Err(ParseDbxError(format!(
            "table content has rows but no <td>/<th> cells: {}",
            truncate(html, 200)
        )));
    }

    Ok(HtmlTable {
        n_rows: occupied.len() as u32,
        n_cols,
        cells,
    })
}

/// Strip tags and decode the basic entities from cell HTML; whitespace runs
/// collapse to single spaces.
fn flatten_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Read an integer attribute (`rowspan="2"`, `colspan=3`) out of a tag body.
fn attr_u32(tag_body: &str, attr: &str) -> Option<u32> {
    let lower = tag_body.to_ascii_lowercase();
    let pos = lower.find(attr)?;
    let rest = &lower[pos + attr.len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_table_with_thead() {
        let table = parse_html_table(
            "<table><thead><tr><th>Metric</th><th>2015</th></tr></thead>\
             <tbody><tr><td>Sales</td><td>10,328</td></tr>\
             <tr><td>Margin &amp; other</td><td>22.9%</td></tr></tbody></table>",
        )
        .unwrap();
        assert_eq!((table.n_rows, table.n_cols), (3, 2));
        assert_eq!(table.cells.len(), 6);
        assert!(table.cells[0].is_header && table.cells[1].is_header);
        assert!(!table.cells[2].is_header);
        assert_eq!(table.cells[2].text, "Sales");
        assert_eq!(table.cells[4].text, "Margin & other");
    }

    #[test]
    fn spans_occupy_the_grid() {
        // 2x3 grid: A spans both rows; the second row's cells shift right.
        let table = parse_html_table(
            "<table><tr><td rowspan=\"2\">A</td><td colspan=\"2\">B</td></tr>\
             <tr><td>C</td><td>D</td></tr></table>",
        )
        .unwrap();
        assert_eq!((table.n_rows, table.n_cols), (2, 3));
        let cell = |text: &str| table.cells.iter().find(|c| c.text == text).unwrap();
        assert_eq!((cell("A").row, cell("A").col, cell("A").row_span), (0, 0, 2));
        assert_eq!((cell("B").row, cell("B").col, cell("B").col_span), (0, 1, 2));
        assert_eq!((cell("C").row, cell("C").col), (1, 1));
        assert_eq!((cell("D").row, cell("D").col), (1, 2));
    }

    #[test]
    fn nested_markup_and_entities_flatten() {
        let table = parse_html_table(
            "<table><tr><td><b>Bold</b> &nbsp; text&#39;s</td></tr></table>",
        )
        .unwrap();
        assert_eq!(table.cells[0].text, "Bold text's");
    }

    #[test]
    fn sloppy_html_without_closing_tags() {
        let table =
            parse_html_table("<table><tr><td>a<td>b<tr><td>c<td>d</table>").unwrap();
        assert_eq!((table.n_rows, table.n_cols), (2, 2));
        assert_eq!(table.cells.len(), 4);
    }

    #[test]
    fn no_rows_is_a_hard_error() {
        let err = parse_html_table("just some text").expect_err("no rows");
        assert!(err.0.contains("no <tr> rows"), "got: {err}");
        let err = parse_html_table("<table><tr></tr></table>").expect_err("no cells");
        assert!(err.0.contains("no <td>/<th> cells"), "got: {err}");
    }
}
