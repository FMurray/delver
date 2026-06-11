//! User-defined TABLE types (Stage C, docs/DECISIONS.md D-021).
//!
//! `TYPE Name AS TABLE ( field TEXT, ... );` declarations compile into a
//! registry ([`TableTypeDef`] keyed by name on the template `Root`);
//! `Table(type="Name")` then coerces every matched detected table into typed
//! records via [`extract_typed_records`].
//!
//! **Data-quality vs fail-loud (D-006 boundary).** Everything in this module
//! operates on *document data*: a cell that cannot be coerced is a DATA issue
//! — the value becomes null, an entry lands in the output's `errors` array
//! (row/col/raw/reason), the `coerced_err` count grows, and the run
//! continues. TYPE/template *misuse* (duplicate TYPE, unsupported field type,
//! undefined TYPE reference, `type=` on a non-Table element) never reaches
//! this module: those are hard template-compile errors in `docql.rs`.
//!
//! **Column mapping** (header pass, then positional fallback):
//! 1. *Filler columns* are excluded entirely: a column is filler when every
//!    body cell is empty or consists solely of currency/percent decoration
//!    characters (`$ % ( )` and whitespace) — the `$` columns SEC filings
//!    interleave between the label and each value column.
//! 2. *Header pass* (only when the table has a detected header row): fields
//!    claim columns in declared order; a field claims the best-scoring
//!    unclaimed non-filler column whose normalized header text is within
//!    Levenshtein distance <= 20% of the longer normalized string
//!    (similarity >= 0.8, evaluated as `5*dist <= max_len` in integer
//!    arithmetic so the boundary is exact; ties resolve to the leftmost
//!    column). Normalization lowercases and strips non-alphanumerics, so
//!    field `y2015` matches header "2015" (distance 1 of 5).
//! 3. *Positional fallback*: fields still unmatched (no header row, or no
//!    column scored above threshold) take the remaining unclaimed non-filler
//!    columns left-to-right in field declaration order.
//! 4. A field with no column left maps to null in every record, recorded as
//!    one table-level `errors` entry (row/col null). Extra unclaimed columns
//!    are ignored.
//!
//! **Cell coercion**: TEXT is verbatim; INT/DECIMAL strip financial-notation
//! conventions: trailing `%` (recorded per cell in `percent_cells`),
//! surrounding parentheses = negative, `$`/`,`/whitespace removed; the
//! remainder parses as i64 / f64. Empty cells are null for every field type
//! (a missing value is a missing value) and count as `coerced_ok`.
//! `coerced_ok + coerced_err == records x mapped fields`.

use serde::Serialize;

use crate::table::TableStructure;

/// Field types supported by `TYPE ... AS TABLE` in v1 (the spec example's
/// set). Unsupported names are a template-compile error listing
/// [`FieldType::SUPPORTED`] (D-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FieldType {
    Text,
    Int,
    Decimal,
}

impl FieldType {
    /// Supported declaration keywords, for fail-loud error messages.
    pub const SUPPORTED: &'static str = "TEXT, INT, DECIMAL";

    /// Keywords are uppercase, matching the `TYPE`/`AS`/`TABLE` keywords.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "TEXT" => Some(Self::Text),
            "INT" => Some(Self::Int),
            "DECIMAL" => Some(Self::Decimal),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "TEXT",
            Self::Int => "INT",
            Self::Decimal => "DECIMAL",
        }
    }
}

/// One declared field of a user-defined table type.
#[derive(Debug, Clone, Serialize)]
pub struct FieldDef {
    pub name: String,
    pub field_type: FieldType,
}

/// One compiled `TYPE Name AS TABLE ( ... );` declaration.
#[derive(Debug, Clone, Serialize)]
pub struct TableTypeDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

/// One data-quality issue met while coercing a table (D-021): a cell that
/// failed coercion (`row`/`col` are the grid coordinates, matching the
/// persisted `table_cells` addressing), or a field with no usable column
/// (`row`/`col` null).
#[derive(Debug, Clone, Serialize)]
pub struct CellError {
    pub row: Option<u32>,
    pub col: Option<u32>,
    pub raw: String,
    pub reason: String,
}

/// Result of coercing one detected table against one type definition.
#[derive(Debug, Clone)]
pub struct TypedExtraction {
    /// One `{field: value}` object per body row, fields in declared order.
    pub records: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Grid row index of each record (parallel to `records`).
    pub source_rows: Vec<u32>,
    pub errors: Vec<CellError>,
    pub coerced_ok: u32,
    pub coerced_err: u32,
    /// (grid row, field name) of every cell whose trailing `%` was stripped.
    pub percent_cells: Vec<(u32, String)>,
}

/// Coerce a detected table into typed records (see the module docs for the
/// column-mapping and coercion rules). Never fails: data problems degrade to
/// nulls + `errors` entries.
pub fn extract_typed_records(table: &TableStructure, ty: &TableTypeDef) -> TypedExtraction {
    let n_cols = table.n_cols as usize;

    // Grid views: per-column header text, body rows as (grid row, texts).
    let header_rows: std::collections::BTreeSet<u32> = table
        .cells
        .iter()
        .filter(|c| c.is_header)
        .map(|c| c.row)
        .collect();
    let first_header_row = header_rows.iter().next().copied();
    let mut header_texts = vec![String::new(); n_cols];
    if let Some(h) = first_header_row {
        for cell in table.cells.iter().filter(|c| c.row == h) {
            if let Some(slot) = header_texts.get_mut(cell.col as usize) {
                *slot = cell.text.clone();
            }
        }
    }
    let mut body: Vec<(u32, Vec<String>)> = (0..table.n_rows)
        .filter(|r| !header_rows.contains(r))
        .map(|r| (r, vec![String::new(); n_cols]))
        .collect();
    for cell in &table.cells {
        if header_rows.contains(&cell.row) {
            continue;
        }
        if let Some((_, texts)) = body.iter_mut().find(|(r, _)| *r == cell.row) {
            if let Some(slot) = texts.get_mut(cell.col as usize) {
                *slot = cell.text.clone();
            }
        }
    }

    // 1. Filler columns: every body cell empty or pure decoration.
    let filler: Vec<bool> = (0..n_cols)
        .map(|col| body.iter().all(|(_, texts)| is_filler_cell(&texts[col])))
        .collect();

    // 2. Header pass + 3. positional fallback.
    let mut claimed = vec![false; n_cols];
    let mut mapping: Vec<Option<usize>> = vec![None; ty.fields.len()];
    if first_header_row.is_some() {
        for (fi, field) in ty.fields.iter().enumerate() {
            let field_norm = normalize(&field.name);
            if field_norm.is_empty() {
                continue;
            }
            let mut best: Option<(usize, f64)> = None;
            for col in 0..n_cols {
                if claimed[col] || filler[col] {
                    continue;
                }
                let header_norm = normalize(&header_texts[col]);
                if header_norm.is_empty() {
                    continue;
                }
                let dist = strsim::levenshtein(&field_norm, &header_norm);
                let max_len = field_norm.chars().count().max(header_norm.chars().count());
                // similarity >= 0.8, exact at the boundary (5*dist <= max_len).
                if 5 * dist > max_len {
                    continue;
                }
                let score = 1.0 - dist as f64 / max_len as f64;
                if best.map_or(true, |(_, s)| score > s) {
                    best = Some((col, score));
                }
            }
            if let Some((col, _)) = best {
                claimed[col] = true;
                mapping[fi] = Some(col);
            }
        }
    }
    let mut next_col = 0usize;
    for slot in mapping.iter_mut() {
        if slot.is_some() {
            continue;
        }
        while next_col < n_cols && (claimed[next_col] || filler[next_col]) {
            next_col += 1;
        }
        if next_col < n_cols {
            claimed[next_col] = true;
            *slot = Some(next_col);
        }
    }

    let mut out = TypedExtraction {
        records: Vec::with_capacity(body.len()),
        source_rows: Vec::with_capacity(body.len()),
        errors: Vec::new(),
        coerced_ok: 0,
        coerced_err: 0,
        percent_cells: Vec::new(),
    };

    // 4. Fields with no usable column: one table-level error each.
    for (fi, field) in ty.fields.iter().enumerate() {
        if mapping[fi].is_none() {
            out.errors.push(CellError {
                row: None,
                col: None,
                raw: String::new(),
                reason: format!(
                    "no usable column for field '{}' ({} non-filler columns, {} fields)",
                    field.name,
                    filler.iter().filter(|f| !**f).count(),
                    ty.fields.len()
                ),
            });
        }
    }

    for (grid_row, texts) in &body {
        let mut record = serde_json::Map::with_capacity(ty.fields.len());
        for (fi, field) in ty.fields.iter().enumerate() {
            let Some(col) = mapping[fi] else {
                record.insert(field.name.clone(), serde_json::Value::Null);
                continue;
            };
            let raw = texts[col].as_str();
            match coerce_cell(raw, field.field_type) {
                Ok(coerced) => {
                    out.coerced_ok += 1;
                    if coerced.percent {
                        out.percent_cells.push((*grid_row, field.name.clone()));
                    }
                    record.insert(field.name.clone(), coerced.value);
                }
                Err(reason) => {
                    out.coerced_err += 1;
                    out.errors.push(CellError {
                        row: Some(*grid_row),
                        col: Some(col as u32),
                        raw: raw.to_string(),
                        reason,
                    });
                    record.insert(field.name.clone(), serde_json::Value::Null);
                }
            }
        }
        out.records.push(record);
        out.source_rows.push(*grid_row);
    }

    out
}

/// A successfully coerced cell value; `percent` is set when a trailing `%`
/// was stripped from a numeric cell.
#[derive(Debug, Clone)]
pub struct CoercedCell {
    pub value: serde_json::Value,
    pub percent: bool,
}

/// Coerce one cell's raw text to `field_type` (module docs list the rules).
/// `Err(reason)` is the data-quality path: callers null the value and record
/// the reason — never abort (D-021).
pub fn coerce_cell(raw: &str, field_type: FieldType) -> Result<CoercedCell, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(CoercedCell {
            value: serde_json::Value::Null,
            percent: false,
        });
    }
    if field_type == FieldType::Text {
        return Ok(CoercedCell {
            value: serde_json::Value::String(raw.to_string()),
            percent: false,
        });
    }

    // Numeric conventions: trailing % first (it sits outside the
    // parentheses in SEC notation, e.g. "(6.0)%"), then surrounding parens
    // = negative, then strip $/,/whitespace.
    let mut s = trimmed;
    let percent = s.ends_with('%');
    if percent {
        s = s[..s.len() - 1].trim_end();
    }
    let negative = s.starts_with('(') && s.ends_with(')') && s.len() >= 2;
    if negative {
        s = s[1..s.len() - 1].trim();
    }
    let cleaned: String = s
        .chars()
        .filter(|c| *c != '$' && *c != ',' && !c.is_whitespace())
        .collect();
    if cleaned.is_empty() {
        return Err(format!(
            "no digits left after stripping conventions from {raw:?}"
        ));
    }

    let value = match field_type {
        FieldType::Int => {
            let n: i64 = cleaned
                .parse()
                .map_err(|_| format!("cannot parse {raw:?} as INT"))?;
            serde_json::Value::Number(serde_json::Number::from(if negative { -n } else { n }))
        }
        FieldType::Decimal => {
            let n: f64 = cleaned
                .parse()
                .map_err(|_| format!("cannot parse {raw:?} as DECIMAL"))?;
            let n = if negative { -n } else { n };
            serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| format!("{raw:?} is not a finite DECIMAL"))?
        }
        FieldType::Text => unreachable!("handled above"),
    };
    Ok(CoercedCell { value, percent })
}

/// Filler-cell test (column heuristic, module docs): empty, or only
/// currency/percent decoration characters and whitespace.
fn is_filler_cell(text: &str) -> bool {
    text.chars()
        .all(|c| matches!(c, '$' | '%' | '(' | ')') || c.is_whitespace())
}

/// Header/field-name normalization for fuzzy matching: lowercase, keep
/// alphanumerics only.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(raw: &str) -> Result<CoercedCell, String> {
        coerce_cell(raw, FieldType::Decimal)
    }

    #[test]
    fn coercion_conventions() {
        assert_eq!(dec("10,328").unwrap().value, serde_json::json!(10328.0));
        assert_eq!(dec("$1,234").unwrap().value, serde_json::json!(1234.0));
        assert_eq!(dec("(56)").unwrap().value, serde_json::json!(-56.0));
        let pct = dec("7.8 %").unwrap();
        assert_eq!(pct.value, serde_json::json!(7.8));
        assert!(pct.percent);
        let neg_pct = dec("(6.0)%").unwrap();
        assert_eq!(neg_pct.value, serde_json::json!(-6.0));
        assert!(neg_pct.percent);
        assert_eq!(dec("").unwrap().value, serde_json::Value::Null);
        assert_eq!(dec("  ").unwrap().value, serde_json::Value::Null);
        assert!(dec("—").is_err(), "em dash is not a number");
        assert!(coerce_cell("7.8", FieldType::Int).is_err());
        assert_eq!(
            coerce_cell("(2,015)", FieldType::Int).unwrap().value,
            serde_json::json!(-2015)
        );
        assert_eq!(
            coerce_cell(" Sales (millions) ", FieldType::Text)
                .unwrap()
                .value,
            serde_json::json!(" Sales (millions) ")
        );
    }

    #[test]
    fn header_similarity_boundary_is_exact() {
        // y2015 vs 2015: distance 1 of 5 => exactly the 0.8 threshold; the
        // integer rule must accept it (f64 1.0 - 1.0/5.0 < 0.8 would not).
        let (f, h) = (normalize("y2015"), normalize("2015"));
        let dist = strsim::levenshtein(&f, &h);
        let max_len = f.chars().count().max(h.chars().count());
        assert_eq!((dist, max_len), (1, 5));
        assert!(5 * dist <= max_len);
        // y2014 vs 2013: distance 2 of 5 => rejected.
        let (f, h) = (normalize("y2014"), normalize("2013"));
        assert!(5 * strsim::levenshtein(&f, &h) > 5);
    }

    #[test]
    fn filler_cells() {
        for s in ["", " ", "$", "%", "$ ", "( )"] {
            assert!(is_filler_cell(s), "{s:?} should be filler");
        }
        for s in ["0.7 %", "(7.3)", "—", "x", "$1"] {
            assert!(!is_filler_cell(s), "{s:?} should not be filler");
        }
    }
}
