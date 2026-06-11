//! TABLE structure detection (Stage B slice 3, docs/DECISIONS.md D-018).
//!
//! Deterministic, parse-time-only detection. Inputs are the page's painted
//! vector paths (PATH aux elements, D-016 — their captured points/bbox and
//! stroke/fill flags exist exactly for this) and the glyph-level
//! [`CellFragment`]s captured during the content-stream walk (runs split at
//! column boundaries; see `parse::split_cell_fragments`). Three strategies,
//! tried in priority order, each consuming the evidence it uses:
//!
//! 1. `ruled`     — cluster straight horizontal/vertical rules (thin painted
//!    lines/rects, the four edges of stroked rects and of cell-background
//!    boxes) into connected grids; >=2 horizontal + >=2 vertical intersecting
//!    rules form a cell lattice; text fragments snap into cells by bbox-center
//!    containment. Collinear rules merge across small gaps so zebra-striped
//!    (alternating row shading) tables form one lattice.
//! 2. `row-ruled` — >=3 evenly stacked horizontal rules spanning a common
//!    x-range (SEC filings often rule rows only); columns are inferred from
//!    text alignment (strategy 3) inside the rule band.
//! 3. `aligned`   — borderless: >=3 consecutive multi-cell text lines whose
//!    cells share >=2 x-aligned column extents (left-edge or right-edge
//!    alignment; right-edge stands in for decimal-point alignment, which it
//!    equals for uniformly formatted numeric columns).
//!
//! Candidates smaller than 2x2 (after dropping fully-empty rows/columns) are
//! rejected. Hydration never re-runs detection: persisted documents
//! round-trip verbatim (D-011/D-016).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::geo::Rect;
use crate::parse::{AuxElement, AuxKind, CellFragment};

// ───────────────────────────── tolerances ─────────────────────────────
// All in PDF points; recorded in DECISIONS.md D-018.

/// A painted path this thin in one dimension (and at least `MIN_RULE_LEN` in
/// the other) is a rule.
const RULE_THICKNESS_MAX: f32 = 2.5;
/// Minimum rule length along its axis.
const MIN_RULE_LEN: f32 = 6.0;
/// Filled axis-aligned rectangles up to this tall count as cell-background
/// boxes (their four edges become rules); taller fills are page decoration.
const CELL_BOX_MAX_H: f32 = 60.0;
/// Cell boxes must not span (almost) the full page width — full-bleed
/// backgrounds are not table evidence.
const CELL_BOX_MAX_W_FRAC: f32 = 0.95;
/// Rule positions (y for horizontal, x for vertical) within this tolerance
/// cluster into one lattice boundary.
const POS_CLUSTER_TOL: f32 = 2.0;
/// Collinear rules merge across gaps up to this size. Sized to bridge the
/// unshaded bands of zebra-striped tables (~9-19pt) while keeping vertically
/// adjacent distinct tables (usually separated by >2 text lines) apart.
const COLLINEAR_MERGE_GAP: f32 = 20.0;
/// Slack along a vertical rule when testing intersection with a horizontal
/// rule (connects under-header rules that sit just above the first row band).
const CONNECT_GAP: f32 = 10.0;
/// A text line ending within this distance above a lattice top may be
/// absorbed as the table's header row (SEC tables set the header above the
/// first ruled/shaded row).
const HEADER_ABSORB_GAP: f32 = 12.0;
/// Fragments whose vertical centers differ by no more than this share a line.
const LINE_TOL: f32 = 3.5;
/// Aligned tables: maximum center-to-center spacing of consecutive lines.
const ROW_GAP_MAX: f32 = 28.0;
/// Left/right edge alignment tolerance for column clustering.
const ALIGN_TOL: f32 = 3.0;
/// Row-ruled: minimum rule length and maximum spacing between stacked rules.
const ROW_RULE_MIN_LEN: f32 = 50.0;
const ROW_RULE_GAP_MAX: f32 = 40.0;
/// Row-ruled "evenly stacked": max gap <= 2.5 x min gap (floored).
const ROW_RULE_EVEN_RATIO: f32 = 2.5;
const ROW_RULE_MIN_GAP_FLOOR: f32 = 4.0;
/// Column extents merge when their x-intervals overlap or sit closer than
/// this.
const EXTENT_MERGE_GAP: f32 = 1.0;
/// Aligned-strategy prose guard: reject a candidate when a single column
/// extent spans more than this fraction of the table width. Bulleted lists
/// ("· " + a prose line) are the canonical false positive — two aligned
/// columns where the text column is ~90% of the band. Real label columns in
/// financial tables stay under ~60%.
const ALIGNED_MAX_COL_FRAC: f32 = 0.75;

// ───────────────────────────── public model ─────────────────────────────

/// How a table was detected. Serialized as `"ruled"` / `"row-ruled"` /
/// `"aligned"` (kebab-case) in metadata and output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TableStrategy {
    Ruled,
    RowRuled,
    Aligned,
}

impl TableStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            TableStrategy::Ruled => "ruled",
            TableStrategy::RowRuled => "row-ruled",
            TableStrategy::Aligned => "aligned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ruled" => Some(TableStrategy::Ruled),
            "row-ruled" => Some(TableStrategy::RowRuled),
            "aligned" => Some(TableStrategy::Aligned),
            _ => None,
        }
    }
}

/// One table cell. `row`/`col` address the grid (0-based); spans default to 1
/// (span *detection* is out of scope this slice — the fields exist so merged
/// cells can be represented by store/consumers without a schema change).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub row: u32,
    pub col: u32,
    pub row_span: u32,
    pub col_span: u32,
    /// (x0, y0, x1, y1) in top-left page coordinates — the grid slot extent.
    pub bbox: (f32, f32, f32, f32),
    pub text: String,
    pub is_header: bool,
}

/// Structured content of one detected table, carried beside its kind=table
/// aux element (the `BlobPayload` pattern from D-016: structure stays out of
/// the metadata JSON; delver-store persists it to `table_cells`).
#[derive(Debug, Clone, PartialEq)]
pub struct TableStructure {
    pub bbox: Rect,
    pub page: u32,
    pub n_rows: u32,
    pub n_cols: u32,
    pub cells: Vec<TableCell>,
    pub strategy: TableStrategy,
    pub confidence: f64,
}

impl TableStructure {
    /// Header row texts (cells of rows flagged `is_header`, first such row),
    /// in column order. Empty when no header was detected.
    pub fn header_texts(&self) -> Vec<String> {
        let Some(header_row) = self
            .cells
            .iter()
            .filter(|c| c.is_header)
            .map(|c| c.row)
            .min()
        else {
            return Vec::new();
        };
        self.row_texts(header_row)
    }

    /// Body rows (rows not flagged header) as text grids, row order.
    pub fn body_rows(&self) -> Vec<Vec<String>> {
        let header_rows: std::collections::BTreeSet<u32> = self
            .cells
            .iter()
            .filter(|c| c.is_header)
            .map(|c| c.row)
            .collect();
        (0..self.n_rows)
            .filter(|r| !header_rows.contains(r))
            .map(|r| self.row_texts(r))
            .collect()
    }

    fn row_texts(&self, row: u32) -> Vec<String> {
        let mut texts = vec![String::new(); self.n_cols as usize];
        for cell in self.cells.iter().filter(|c| c.row == row) {
            if let Some(slot) = texts.get_mut(cell.col as usize) {
                *slot = cell.text.clone();
            }
        }
        texts
    }

    /// The table-level metadata persisted on the element row
    /// (`n_rows`/`n_cols`/`strategy`/`confidence`, D-018).
    pub fn element_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "n_rows": self.n_rows,
            "n_cols": self.n_cols,
            "strategy": self.strategy.as_str(),
            "confidence": self.confidence,
        })
    }
}

// ───────────────────────────── internal types ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    H,
    V,
}

/// A straight axis-aligned rule: `pos` is y for horizontal / x for vertical;
/// `lo..hi` is its extent along the other axis.
#[derive(Debug, Clone, Copy)]
struct Rule {
    axis: Axis,
    pos: f32,
    lo: f32,
    hi: f32,
}

impl Rule {
    fn len(&self) -> f32 {
        self.hi - self.lo
    }
}

/// One text cell candidate: line-merged fragments.
#[derive(Debug, Clone)]
struct LineCell {
    bbox: (f32, f32, f32, f32),
    text: String,
    font_size: f32,
    font_name: Option<String>,
    used: bool,
}

impl LineCell {
    fn cx(&self) -> f32 {
        (self.bbox.0 + self.bbox.2) * 0.5
    }
    fn cy(&self) -> f32 {
        (self.bbox.1 + self.bbox.3) * 0.5
    }
}

/// One visual text line (indices into the cell vec, sorted by x).
#[derive(Debug, Clone)]
struct Line {
    y0: f32,
    y1: f32,
    cy: f32,
    cells: Vec<usize>,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn median(values: &mut Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

// ───────────────────────────── entry point ─────────────────────────────

/// Detect tables on one page. `fragments` and `paths` must already be in
/// top-left page coordinates. Returns kind=table aux elements (sorted by
/// position) each carrying its [`TableStructure`].
pub fn detect_tables_on_page(
    fragments: &[CellFragment],
    aux_elements: &[AuxElement],
    page_number: u32,
    page_width: f32,
) -> Vec<AuxElement> {
    let (mut cells, lines) = build_lines(fragments);
    let rules = extract_rules(aux_elements, page_width);
    let (h_rules, v_rules) = merge_rules(rules);

    let mut tables: Vec<TableStructure> = Vec::new();
    let mut used_h = vec![false; h_rules.len()];
    let mut used_v = vec![false; v_rules.len()];

    detect_ruled(
        &h_rules,
        &v_rules,
        &mut used_h,
        &mut used_v,
        &mut cells,
        &lines,
        page_number,
        &mut tables,
    );
    detect_row_ruled(
        &h_rules,
        &mut used_h,
        &mut cells,
        &lines,
        page_number,
        &mut tables,
    );
    detect_aligned(&mut cells, &lines, page_number, &mut tables);

    tables.sort_by(|a, b| {
        a.bbox
            .y0
            .total_cmp(&b.bbox.y0)
            .then(a.bbox.x0.total_cmp(&b.bbox.x0))
    });

    tables
        .into_iter()
        .map(|table| AuxElement {
            id: Uuid::new_v4(),
            kind: AuxKind::Table,
            page_number,
            bbox: table.bbox,
            text: None,
            metadata: table.element_metadata(),
            blob: None,
            table: Some(table),
        })
        .collect()
}

// ───────────────────────── lines & fragments ─────────────────────────

/// Group fragments into visual lines (y-center clustering) and merge
/// horizontally adjacent fragments into cell candidates. Fragments were
/// already split at column boundaries by the parser; the merge here only
/// reunites pieces with near-zero gaps (e.g. a currency symbol drawn flush
/// against its number).
fn build_lines(fragments: &[CellFragment]) -> (Vec<LineCell>, Vec<Line>) {
    let mut frags: Vec<&CellFragment> = fragments
        .iter()
        .filter(|f| !f.text.trim().is_empty())
        .collect();
    frags.sort_by(|a, b| {
        let ay = (a.bbox.1 + a.bbox.3) * 0.5;
        let by = (b.bbox.1 + b.bbox.3) * 0.5;
        ay.total_cmp(&by).then(a.bbox.0.total_cmp(&b.bbox.0))
    });

    // Cluster into lines.
    let mut line_groups: Vec<Vec<&CellFragment>> = Vec::new();
    let mut current_cy = f32::MIN;
    for frag in frags {
        let cy = (frag.bbox.1 + frag.bbox.3) * 0.5;
        if line_groups.is_empty() || cy - current_cy > LINE_TOL {
            line_groups.push(vec![frag]);
        } else {
            line_groups.last_mut().expect("non-empty").push(frag);
        }
        current_cy = cy;
    }

    let mut cells: Vec<LineCell> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();

    for mut group in line_groups.into_iter() {
        group.sort_by(|a, b| a.bbox.0.total_cmp(&b.bbox.0));
        // Merge gap: ~half a character height, so single spaces (already kept
        // inside fragments) and flush-adjacent pieces merge while real column
        // gaps stay split. Matches the parser's split threshold.
        let mut heights: Vec<f32> = group.iter().map(|f| f.bbox.3 - f.bbox.1).collect();
        let h_med = median(&mut heights);
        let merge_gap = (0.45 * h_med).clamp(2.0, 7.0);

        let mut line_cells: Vec<usize> = Vec::new();
        let mut current: Option<LineCell> = None;
        for frag in group {
            match current.as_mut() {
                Some(cell) if frag.bbox.0 - cell.bbox.2 <= merge_gap => {
                    if frag.bbox.0 - cell.bbox.2 >= 1.0 {
                        cell.text.push(' ');
                    }
                    cell.text.push_str(frag.text.trim());
                    cell.bbox.0 = cell.bbox.0.min(frag.bbox.0);
                    cell.bbox.1 = cell.bbox.1.min(frag.bbox.1);
                    cell.bbox.2 = cell.bbox.2.max(frag.bbox.2);
                    cell.bbox.3 = cell.bbox.3.max(frag.bbox.3);
                }
                _ => {
                    if let Some(done) = current.take() {
                        line_cells.push(cells.len());
                        cells.push(done);
                    }
                    current = Some(LineCell {
                        bbox: frag.bbox,
                        text: frag.text.trim().to_string(),
                        font_size: frag.font_size,
                        font_name: frag.font_name.clone(),
                        used: false,
                    });
                }
            }
        }
        if let Some(done) = current.take() {
            line_cells.push(cells.len());
            cells.push(done);
        }

        if line_cells.is_empty() {
            continue;
        }
        let y0 = line_cells
            .iter()
            .map(|&i| cells[i].bbox.1)
            .fold(f32::MAX, f32::min);
        let y1 = line_cells
            .iter()
            .map(|&i| cells[i].bbox.3)
            .fold(f32::MIN, f32::max);
        lines.push(Line {
            y0,
            y1,
            cy: (y0 + y1) * 0.5,
            cells: line_cells,
        });
    }

    (cells, lines)
}

// ───────────────────────────── rules ─────────────────────────────

/// Turn painted paths into axis-aligned rules. Sources, in order of
/// precedence per path: thin bbox (whole path is one rule); axis-aligned
/// rectangle (stroked, or filled within cell-box size limits → its four
/// edges); otherwise per-segment extraction from the captured points.
fn extract_rules(aux_elements: &[AuxElement], page_width: f32) -> Vec<Rule> {
    let mut rules = Vec::new();
    for aux in aux_elements.iter().filter(|a| a.kind == AuxKind::Path) {
        let b = aux.bbox;
        let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
        let stroke = aux.metadata["stroke"].as_bool().unwrap_or(false);
        let fill = aux.metadata["fill"].as_bool().unwrap_or(false);
        if !stroke && !fill {
            continue;
        }

        if h <= RULE_THICKNESS_MAX && w >= MIN_RULE_LEN {
            rules.push(Rule {
                axis: Axis::H,
                pos: (b.y0 + b.y1) * 0.5,
                lo: b.x0,
                hi: b.x1,
            });
            continue;
        }
        if w <= RULE_THICKNESS_MAX && h >= MIN_RULE_LEN {
            rules.push(Rule {
                axis: Axis::V,
                pos: (b.x0 + b.x1) * 0.5,
                lo: b.y0,
                hi: b.y1,
            });
            continue;
        }

        let points = path_points(aux);
        let is_rect = is_axis_aligned_rect(&points, &b);
        let cell_box = fill && h <= CELL_BOX_MAX_H && w <= CELL_BOX_MAX_W_FRAC * page_width;
        if is_rect && (stroke || cell_box) {
            rules.push(Rule {
                axis: Axis::H,
                pos: b.y0,
                lo: b.x0,
                hi: b.x1,
            });
            rules.push(Rule {
                axis: Axis::H,
                pos: b.y1,
                lo: b.x0,
                hi: b.x1,
            });
            rules.push(Rule {
                axis: Axis::V,
                pos: b.x0,
                lo: b.y0,
                hi: b.y1,
            });
            rules.push(Rule {
                axis: Axis::V,
                pos: b.x1,
                lo: b.y0,
                hi: b.y1,
            });
            continue;
        }

        // Multi-segment path (e.g. a whole grid stroked as one path): treat
        // consecutive captured points as segments and keep the axis-aligned
        // ones. Move-to jumps between rules are almost never axis-aligned, so
        // this stays conservative. Points are capped at 32 per path (D-016) —
        // truncated paths contribute what was captured.
        for pair in points.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            if (y1 - y0).abs() <= 1.5 && (x1 - x0).abs() >= MIN_RULE_LEN {
                rules.push(Rule {
                    axis: Axis::H,
                    pos: (y0 + y1) * 0.5,
                    lo: x0.min(x1),
                    hi: x0.max(x1),
                });
            } else if (x1 - x0).abs() <= 1.5 && (y1 - y0).abs() >= MIN_RULE_LEN {
                rules.push(Rule {
                    axis: Axis::V,
                    pos: (x0 + x1) * 0.5,
                    lo: y0.min(y1),
                    hi: y0.max(y1),
                });
            }
        }
    }
    rules
}

fn path_points(aux: &AuxElement) -> Vec<(f32, f32)> {
    aux.metadata["points"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let pt = p.as_array()?;
                    Some((pt.first()?.as_f64()? as f32, pt.get(1)?.as_f64()? as f32))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// All captured points sit near corners of the bbox, and all four corners are
/// hit — the path is an axis-aligned rectangle (a `re` or an m/l box).
fn is_axis_aligned_rect(points: &[(f32, f32)], b: &Rect) -> bool {
    if !(4..=8).contains(&points.len()) {
        return false;
    }
    let near = |a: f32, b: f32| (a - b).abs() <= 1.0;
    let mut corner_hit = [false; 4];
    for &(x, y) in points {
        let on_x = near(x, b.x0) || near(x, b.x1);
        let on_y = near(y, b.y0) || near(y, b.y1);
        if !(on_x && on_y) {
            return false;
        }
        let xi = usize::from(near(x, b.x1));
        let yi = usize::from(near(y, b.y1));
        corner_hit[xi * 2 + yi] = true;
    }
    corner_hit.iter().all(|&hit| hit)
}

/// Cluster rules per axis by position (tolerance `POS_CLUSTER_TOL`), then
/// merge collinear intervals across gaps up to `COLLINEAR_MERGE_GAP`.
/// Returns (horizontal, vertical), each sorted by (pos, lo).
fn merge_rules(rules: Vec<Rule>) -> (Vec<Rule>, Vec<Rule>) {
    let mut out = (Vec::new(), Vec::new());
    for axis in [Axis::H, Axis::V] {
        let mut axis_rules: Vec<Rule> = rules.iter().copied().filter(|r| r.axis == axis).collect();
        axis_rules.sort_by(|a, b| a.pos.total_cmp(&b.pos).then(a.lo.total_cmp(&b.lo)));

        // Position clusters.
        let mut clusters: Vec<Vec<Rule>> = Vec::new();
        for rule in axis_rules {
            match clusters.last_mut() {
                Some(cluster)
                    if rule.pos - cluster.last().expect("non-empty").pos <= POS_CLUSTER_TOL =>
                {
                    cluster.push(rule);
                }
                _ => clusters.push(vec![rule]),
            }
        }

        let merged: &mut Vec<Rule> = if axis == Axis::H { &mut out.0 } else { &mut out.1 };
        for cluster in clusters {
            let pos = cluster.iter().map(|r| r.pos).sum::<f32>() / cluster.len() as f32;
            let mut intervals: Vec<(f32, f32)> = cluster.iter().map(|r| (r.lo, r.hi)).collect();
            intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut current: Option<(f32, f32)> = None;
            for (lo, hi) in intervals {
                match current.as_mut() {
                    Some(span) if lo - span.1 <= COLLINEAR_MERGE_GAP => span.1 = span.1.max(hi),
                    _ => {
                        if let Some((lo, hi)) = current.take() {
                            merged.push(Rule { axis, pos, lo, hi });
                        }
                        current = Some((lo, hi));
                    }
                }
            }
            if let Some((lo, hi)) = current.take() {
                merged.push(Rule { axis, pos, lo, hi });
            }
        }
        merged.sort_by(|a, b| a.pos.total_cmp(&b.pos).then(a.lo.total_cmp(&b.lo)));
    }
    out
}

// ───────────────────────────── strategy: ruled ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn detect_ruled(
    h_rules: &[Rule],
    v_rules: &[Rule],
    used_h: &mut [bool],
    used_v: &mut [bool],
    cells: &mut [LineCell],
    lines: &[Line],
    page_number: u32,
    tables: &mut Vec<TableStructure>,
) {
    if h_rules.len() < 2 || v_rules.len() < 2 {
        return;
    }

    // Union-find over h (0..nh) and v (nh..nh+nv) rules, connected when they
    // intersect (with CONNECT_GAP slack along the vertical rule, so
    // under-header rules sitting just above the first row band still join).
    let nh = h_rules.len();
    let mut parent: Vec<usize> = (0..nh + v_rules.len()).collect();
    fn find(parent: &mut Vec<usize>, i: usize) -> usize {
        if parent[i] != i {
            let root = find(parent, parent[i]);
            parent[i] = root;
        }
        parent[i]
    }
    for (hi, h) in h_rules.iter().enumerate() {
        for (vi, v) in v_rules.iter().enumerate() {
            let x_ok = v.pos >= h.lo - POS_CLUSTER_TOL && v.pos <= h.hi + POS_CLUSTER_TOL;
            let y_ok = h.pos >= v.lo - CONNECT_GAP && h.pos <= v.hi + CONNECT_GAP;
            if x_ok && y_ok {
                let (a, b) = (find(&mut parent, hi), find(&mut parent, nh + vi));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }

    let mut components: std::collections::BTreeMap<usize, (Vec<usize>, Vec<usize>)> =
        std::collections::BTreeMap::new();
    for hi in 0..nh {
        let root = find(&mut parent, hi);
        components.entry(root).or_default().0.push(hi);
    }
    for vi in 0..v_rules.len() {
        let root = find(&mut parent, nh + vi);
        components.entry(root).or_default().1.push(vi);
    }

    for (h_idx, v_idx) in components.values() {
        if h_idx.len() < 2 || v_idx.len() < 2 {
            continue;
        }
        let ys = cluster_positions(h_idx.iter().map(|&i| h_rules[i].pos));
        let xs = cluster_positions(v_idx.iter().map(|&i| v_rules[i].pos));
        if ys.len() < 2 || xs.len() < 2 {
            continue;
        }

        if let Some(table) =
            build_lattice_table(&ys, &xs, cells, lines, page_number, TableStrategy::Ruled)
        {
            for &i in h_idx {
                used_h[i] = true;
            }
            for &i in v_idx {
                used_v[i] = true;
            }
            consume_cells_in_bbox(cells, &table.bbox);
            tables.push(table);
        }
    }
}

/// Cluster scalar positions within `POS_CLUSTER_TOL`; returns sorted cluster
/// means (the lattice boundaries).
fn cluster_positions(values: impl Iterator<Item = f32>) -> Vec<f32> {
    let mut sorted: Vec<f32> = values.collect();
    sorted.sort_by(f32::total_cmp);
    let mut clusters: Vec<Vec<f32>> = Vec::new();
    for v in sorted {
        match clusters.last_mut() {
            Some(cluster) if v - *cluster.last().expect("non-empty") <= POS_CLUSTER_TOL => {
                cluster.push(v)
            }
            _ => clusters.push(vec![v]),
        }
    }
    clusters
        .into_iter()
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect()
}

/// Build a table from lattice boundaries: snap unused cells by center, absorb
/// at most one header line just above the lattice, drop fully-empty
/// rows/columns, apply the 2x2 floor, and compute confidence/header flags.
fn build_lattice_table(
    ys: &[f32],
    xs: &[f32],
    cells: &mut [LineCell],
    lines: &[Line],
    page_number: u32,
    strategy: TableStrategy,
) -> Option<TableStructure> {
    let (x_min, x_max) = (xs[0], *xs.last().expect("non-empty"));
    let (y_min, y_max) = (ys[0], *ys.last().expect("non-empty"));

    // Grid slot (row, col, cell indices). Lattice rows are 1..ys.len()-1.
    let lattice_rows = ys.len() - 1;
    let lattice_cols = xs.len() - 1;
    let mut grid: Vec<Vec<Vec<usize>>> = vec![vec![Vec::new(); lattice_cols]; lattice_rows + 1];

    let snap = |boundaries: &[f32], v: f32| -> Option<usize> {
        if v < boundaries[0] || v > *boundaries.last().expect("non-empty") {
            return None;
        }
        let idx = boundaries.partition_point(|&b| b <= v);
        Some((idx - 1).min(boundaries.len() - 2))
    };

    for (idx, cell) in cells.iter().enumerate() {
        if cell.used {
            continue;
        }
        let (Some(col), Some(row)) = (snap(xs, cell.cx()), snap(ys, cell.cy())) else {
            continue;
        };
        grid[row + 1][col].push(idx);
    }

    // Header absorption: the nearest line fully above the lattice whose cells
    // fall inside the table's x-range and hit >=2 distinct columns.
    let mut header_band: Option<(f32, f32)> = None;
    for line in lines.iter().rev() {
        if line.cy >= y_min {
            continue;
        }
        if y_min - line.y1 > HEADER_ABSORB_GAP {
            continue;
        }
        let line_cells: Vec<usize> = line
            .cells
            .iter()
            .copied()
            .filter(|&i| !cells[i].used)
            .collect();
        if line_cells.is_empty() {
            continue;
        }
        let inside = line_cells
            .iter()
            .all(|&i| cells[i].cx() >= x_min - 6.0 && cells[i].cx() <= x_max + 6.0);
        if !inside {
            continue;
        }
        let mut cols_hit: Vec<usize> = line_cells
            .iter()
            .filter_map(|&i| snap(xs, cells[i].cx()))
            .collect();
        cols_hit.sort_unstable();
        cols_hit.dedup();
        if cols_hit.len() < 2 {
            continue;
        }
        for &i in &line_cells {
            if let Some(col) = snap(xs, cells[i].cx()) {
                grid[0][col].push(i);
            }
        }
        header_band = Some((line.y0, y_min));
        break;
    }

    let has_header_row = header_band.is_some();
    let row_range: Vec<usize> = if has_header_row {
        (0..=lattice_rows).collect()
    } else {
        (1..=lattice_rows).collect()
    };

    // Texts per kept grid slot.
    let row_text = |r: usize, c: usize, cells: &[LineCell]| -> String {
        let mut idxs = grid[r][c].clone();
        idxs.sort_by(|&a, &b| {
            cells[a]
                .cy()
                .total_cmp(&cells[b].cy())
                .then(cells[a].bbox.0.total_cmp(&cells[b].bbox.0))
        });
        let texts: Vec<&str> = idxs.iter().map(|&i| cells[i].text.as_str()).collect();
        texts.join(" ").trim().to_string()
    };

    // Drop fully-empty rows and columns.
    let kept_rows: Vec<usize> = row_range
        .iter()
        .copied()
        .filter(|&r| (0..lattice_cols).any(|c| !grid[r][c].is_empty()))
        .collect();
    let kept_cols: Vec<usize> = (0..lattice_cols)
        .filter(|&c| kept_rows.iter().any(|&r| !grid[r][c].is_empty()))
        .collect();
    if kept_rows.len() < 2 || kept_cols.len() < 2 {
        return None;
    }

    let row_band = |r: usize| -> (f32, f32) {
        if r == 0 {
            header_band.expect("row 0 only exists when header was absorbed")
        } else {
            (ys[r - 1], ys[r])
        }
    };

    let mut out_cells = Vec::new();
    let mut occupied = 0usize;
    for (new_r, &r) in kept_rows.iter().enumerate() {
        for (new_c, &c) in kept_cols.iter().enumerate() {
            let text = row_text(r, c, cells);
            if !text.is_empty() {
                occupied += 1;
            }
            let (by0, by1) = row_band(r);
            out_cells.push(TableCell {
                row: new_r as u32,
                col: new_c as u32,
                row_span: 1,
                col_span: 1,
                bbox: (xs[c], by0, xs[c + 1], by1),
                text,
                is_header: false,
            });
        }
    }

    let n_rows = kept_rows.len() as u32;
    let n_cols = kept_cols.len() as u32;

    // Header heuristic (D-018): an absorbed above-lattice line is the header;
    // otherwise the first row is the header when its dominant font (name or
    // size) differs from the body's.
    let header = if has_header_row && kept_rows.first() == Some(&0) {
        true
    } else {
        first_row_font_differs(&grid, &kept_rows, &kept_cols, cells)
    };
    if header {
        for cell in out_cells.iter_mut().filter(|c| c.row == 0) {
            cell.is_header = true;
        }
    }

    let occupancy = occupied as f64 / (n_rows as f64 * n_cols as f64);
    let confidence = round3((0.7 + 0.3 * occupancy).min(1.0));

    let bbox_y0 = header_band.map_or(y_min, |(y0, _)| y0.min(y_min));
    Some(TableStructure {
        bbox: Rect {
            x0: x_min,
            y0: bbox_y0,
            x1: x_max,
            y1: y_max,
        },
        page: page_number,
        n_rows,
        n_cols,
        cells: out_cells,
        strategy,
        confidence,
    })
}

/// Dominant (font name, font size) of the first kept row vs the rest; ties
/// break lexicographically for determinism.
fn first_row_font_differs(
    grid: &[Vec<Vec<usize>>],
    kept_rows: &[usize],
    kept_cols: &[usize],
    cells: &[LineCell],
) -> bool {
    let style_of = |idx: usize| -> (String, i64) {
        (
            cells[idx]
                .font_name
                .clone()
                .unwrap_or_default()
                .to_lowercase(),
            (cells[idx].font_size * 10.0).round() as i64,
        )
    };
    let dominant = |idxs: &[usize]| -> Option<(String, i64)> {
        let mut counts: std::collections::BTreeMap<(String, i64), usize> =
            std::collections::BTreeMap::new();
        for &i in idxs {
            *counts.entry(style_of(i)).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(k, _)| k)
    };

    let (Some(&first), rest) = (kept_rows.first(), &kept_rows[1.min(kept_rows.len())..]) else {
        return false;
    };
    let first_idxs: Vec<usize> = kept_cols
        .iter()
        .flat_map(|&c| grid[first][c].iter().copied())
        .collect();
    let body_idxs: Vec<usize> = rest
        .iter()
        .flat_map(|&r| kept_cols.iter().flat_map(move |&c| grid[r][c].iter().copied()))
        .collect();
    match (dominant(&first_idxs), dominant(&body_idxs)) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

fn consume_cells_in_bbox(cells: &mut [LineCell], bbox: &Rect) {
    for cell in cells.iter_mut() {
        if !cell.used
            && cell.cx() >= bbox.x0
            && cell.cx() <= bbox.x1
            && cell.cy() >= bbox.y0
            && cell.cy() <= bbox.y1
        {
            cell.used = true;
        }
    }
}

// ─────────────────────── strategy: row-ruled ───────────────────────

fn detect_row_ruled(
    h_rules: &[Rule],
    used_h: &mut [bool],
    cells: &mut [LineCell],
    lines: &[Line],
    page_number: u32,
    tables: &mut Vec<TableStructure>,
) {
    // Candidate rules: unused, long enough, sorted by y.
    let mut candidates: Vec<usize> = (0..h_rules.len())
        .filter(|&i| !used_h[i] && h_rules[i].len() >= ROW_RULE_MIN_LEN)
        .collect();
    candidates.sort_by(|&a, &b| h_rules[a].pos.total_cmp(&h_rules[b].pos));

    let mut in_band = vec![false; h_rules.len()];
    let mut bands: Vec<Vec<usize>> = Vec::new();
    for (seed_order, &seed) in candidates.iter().enumerate() {
        if in_band[seed] {
            continue;
        }
        let seed_rule = h_rules[seed];
        let mut band = vec![seed];
        let mut last_pos = seed_rule.pos;
        for &next in candidates.iter().skip(seed_order + 1) {
            if in_band[next] {
                continue;
            }
            let rule = h_rules[next];
            if rule.pos - last_pos > ROW_RULE_GAP_MAX {
                break;
            }
            let overlap = rule.hi.min(seed_rule.hi) - rule.lo.max(seed_rule.lo);
            if overlap / rule.len().min(seed_rule.len()) < 0.8 {
                continue;
            }
            band.push(next);
            last_pos = rule.pos;
        }
        if band.len() >= 3 {
            for &i in &band {
                in_band[i] = true;
            }
            bands.push(band);
        }
    }

    for band in bands {
        let positions: Vec<f32> = band.iter().map(|&i| h_rules[i].pos).collect();
        let mut gaps: Vec<f32> = positions.windows(2).map(|w| w[1] - w[0]).collect();
        let max_gap = gaps.iter().copied().fold(f32::MIN, f32::max);
        let min_gap = gaps
            .iter()
            .copied()
            .fold(f32::MAX, f32::min)
            .max(ROW_RULE_MIN_GAP_FLOOR);
        if max_gap > ROW_RULE_EVEN_RATIO * min_gap {
            continue; // not evenly stacked
        }
        let median_gap = median(&mut gaps);

        let band_lo = band.iter().map(|&i| h_rules[i].lo).fold(f32::MAX, f32::min);
        let band_hi = band.iter().map(|&i| h_rules[i].hi).fold(f32::MIN, f32::max);
        let y_top = positions[0] - median_gap;
        let y_bot = positions[positions.len() - 1] + 2.0;

        // Text lines inside the band (a row of text typically sits above the
        // rule that closes it; the topmost row may sit above the first rule).
        let band_lines: Vec<&Line> = lines
            .iter()
            .filter(|l| l.cy >= y_top && l.cy <= y_bot)
            .filter(|l| {
                l.cells.iter().any(|&i| {
                    !cells[i].used
                        && cells[i].cx() >= band_lo - 10.0
                        && cells[i].cx() <= band_hi + 10.0
                })
            })
            .collect();
        if band_lines.len() < 2 {
            continue;
        }

        let confidence_base = 0.55;
        if let Some(mut table) = build_line_table(
            &band_lines,
            cells,
            page_number,
            TableStrategy::RowRuled,
            confidence_base,
            0.35,
            2, // row-ruled: >=2 text rows (the rules themselves prove rows)
        ) {
            // Rows above the first rule are header rows (rule-separated).
            let first_rule_y = positions[0];
            let mut header_rows: Vec<u32> = Vec::new();
            for (row_idx, line) in band_lines.iter().enumerate() {
                if line.cy < first_rule_y {
                    header_rows.push(row_idx as u32);
                }
            }
            if !header_rows.is_empty() {
                for cell in table.cells.iter_mut() {
                    cell.is_header = header_rows.contains(&cell.row);
                }
            }
            if band.len() >= 4 {
                table.confidence = round3((table.confidence + 0.05).min(0.95));
            }
            // Extend bbox to the rule extents.
            table.bbox.x0 = table.bbox.x0.min(band_lo);
            table.bbox.x1 = table.bbox.x1.max(band_hi);
            table.bbox.y1 = table.bbox.y1.max(positions[positions.len() - 1]);
            for &i in &band {
                used_h[i] = true;
            }
            consume_cells_in_bbox(cells, &table.bbox);
            tables.push(table);
        }
    }
}

// ─────────────────────── strategy: aligned ───────────────────────

fn detect_aligned(
    cells: &mut [LineCell],
    lines: &[Line],
    page_number: u32,
    tables: &mut Vec<TableStructure>,
) {
    // Maximal runs of consecutive multi-cell lines.
    let mut runs: Vec<Vec<&Line>> = Vec::new();
    let mut current: Vec<&Line> = Vec::new();
    for line in lines {
        let unused = line.cells.iter().filter(|&&i| !cells[i].used).count();
        let qualifies = unused >= 2;
        let close = current
            .last()
            .map_or(true, |prev| line.cy - prev.cy <= ROW_GAP_MAX);
        if qualifies && close {
            current.push(line);
        } else {
            if current.len() >= 3 {
                runs.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            if qualifies {
                current.push(line);
            }
        }
    }
    if current.len() >= 3 {
        runs.push(current);
    }

    for run in runs {
        if let Some(table) = build_line_table(
            &run,
            cells,
            page_number,
            TableStrategy::Aligned,
            0.4,
            0.4,
            3, // aligned: >=3 consecutive lines required
        ) {
            consume_cells_in_bbox(cells, &table.bbox);
            tables.push(table);
        }
    }
}

// ─────────────── shared: line-based table construction ───────────────

/// Column extents over a set of lines: merge the x-intervals of all unused
/// cells; a column is *valid* when supported by enough lines and its cells
/// are left- or right-edge aligned within `ALIGN_TOL`.
struct Columns {
    extents: Vec<(f32, f32)>,
    valid: Vec<bool>,
}

fn infer_columns(line_cells: &[Vec<usize>], cells: &[LineCell]) -> Option<Columns> {
    let n_lines = line_cells.len();
    let mut intervals: Vec<(f32, f32, usize)> = Vec::new(); // (x0, x1, line)
    for (line_idx, idxs) in line_cells.iter().enumerate() {
        for &i in idxs {
            intervals.push((cells[i].bbox.0, cells[i].bbox.2, line_idx));
        }
    }
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut extents: Vec<(f32, f32)> = Vec::new();
    for &(x0, x1, _) in &intervals {
        match extents.last_mut() {
            Some(ext) if x0 - ext.1 <= EXTENT_MERGE_GAP => ext.1 = ext.1.max(x1),
            _ => extents.push((x0, x1)),
        }
    }
    if extents.len() < 2 {
        return None;
    }

    let support_needed = 2usize.max((0.6 * n_lines as f64).ceil() as usize);
    let valid: Vec<bool> = extents
        .iter()
        .map(|&(lo, hi)| {
            let members: Vec<&(f32, f32, usize)> = intervals
                .iter()
                .filter(|&&(x0, x1, _)| (x0 + x1) * 0.5 >= lo && (x0 + x1) * 0.5 <= hi)
                .collect();
            let mut member_lines: Vec<usize> = members.iter().map(|m| m.2).collect();
            member_lines.sort_unstable();
            member_lines.dedup();
            if member_lines.len() < support_needed {
                return false;
            }
            let mut lefts: Vec<f32> = members.iter().map(|m| m.0).collect();
            let mut rights: Vec<f32> = members.iter().map(|m| m.1).collect();
            let left_med = median(&mut lefts);
            let right_med = median(&mut rights);
            let aligned_count = |edges: &[f32], med: f32| {
                edges.iter().filter(|&&e| (e - med).abs() <= ALIGN_TOL).count()
            };
            let needed = ((0.8 * members.len() as f64).ceil() as usize).max(1);
            aligned_count(&lefts, left_med) >= needed || aligned_count(&rights, right_med) >= needed
        })
        .collect();

    Some(Columns { extents, valid })
}

/// Build a table whose rows are text lines and whose columns are inferred
/// extents. Shared by row-ruled (rows from the rule band's lines) and aligned
/// (rows from a consecutive multi-cell line run). Returns None unless >=2
/// valid aligned columns exist, >=3 consecutive lines hit >=2 valid columns
/// (`min_rows` lines for row-ruled, where the rules already prove rows), and
/// the final grid clears the 2x2 floor.
fn build_line_table(
    run: &[&Line],
    cells: &[LineCell],
    page_number: u32,
    strategy: TableStrategy,
    confidence_base: f64,
    confidence_align_weight: f64,
    min_rows: usize,
) -> Option<TableStructure> {
    let line_cells: Vec<Vec<usize>> = run
        .iter()
        .map(|line| {
            line.cells
                .iter()
                .copied()
                .filter(|&i| !cells[i].used)
                .collect()
        })
        .collect();

    let columns = infer_columns(&line_cells, cells)?;
    let n_valid = columns.valid.iter().filter(|&&v| v).count();
    if n_valid < 2 {
        return None;
    }

    // Prose guard (aligned only — ruled/row-ruled candidates carry painted
    // evidence): a column spanning most of the band is body text, not a
    // table column (bulleted lists, hanging-indent paragraphs).
    if strategy == TableStrategy::Aligned {
        let band_width = columns.extents.last().expect("non-empty").1 - columns.extents[0].0;
        let widest = columns
            .extents
            .iter()
            .map(|&(lo, hi)| hi - lo)
            .fold(f32::MIN, f32::max);
        if band_width <= 0.0 || widest > ALIGNED_MAX_COL_FRAC * band_width {
            return None;
        }
    }

    // Per line: count of valid columns hit.
    let col_of = |cell: &LineCell| -> Option<usize> {
        let cx = cell.cx();
        columns
            .extents
            .iter()
            .position(|&(lo, hi)| cx >= lo - EXTENT_MERGE_GAP && cx <= hi + EXTENT_MERGE_GAP)
    };
    let hits_per_line: Vec<usize> = line_cells
        .iter()
        .map(|idxs| {
            let mut hit: Vec<usize> = idxs
                .iter()
                .filter_map(|&i| col_of(&cells[i]))
                .filter(|&c| columns.valid[c])
                .collect();
            hit.sort_unstable();
            hit.dedup();
            hit.len()
        })
        .collect();

    // The alignment-sharing requirement: a streak of >= min_streak
    // consecutive lines each hitting >=2 valid columns.
    let min_streak = min_rows.max(2).min(3);
    let mut best_streak = 0usize;
    let mut streak = 0usize;
    for &hits in &hits_per_line {
        if hits >= 2 {
            streak += 1;
            best_streak = best_streak.max(streak);
        } else {
            streak = 0;
        }
    }
    if best_streak < min_streak || run.len() < min_rows {
        return None;
    }

    let n_cols = columns.extents.len();
    let mut out_cells = Vec::new();
    for (row_idx, (line, idxs)) in run.iter().zip(&line_cells).enumerate() {
        let mut texts: Vec<Vec<usize>> = vec![Vec::new(); n_cols];
        for &i in idxs {
            if let Some(c) = col_of(&cells[i]) {
                texts[c].push(i);
            }
        }
        for (c, mut members) in texts.into_iter().enumerate() {
            members.sort_by(|&a, &b| cells[a].bbox.0.total_cmp(&cells[b].bbox.0));
            let text = members
                .iter()
                .map(|&i| cells[i].text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            out_cells.push(TableCell {
                row: row_idx as u32,
                col: c as u32,
                row_span: 1,
                col_span: 1,
                bbox: (columns.extents[c].0, line.y0, columns.extents[c].1, line.y1),
                text,
                is_header: false,
            });
        }
    }

    let n_rows = run.len();
    if n_rows < 2 || n_cols < 2 {
        return None;
    }

    // Font-based header heuristic (rule-based header flags are applied by the
    // row-ruled caller on top of this).
    let row0_styles: Vec<usize> = line_cells[0].clone();
    let body_styles: Vec<usize> = line_cells[1..].iter().flatten().copied().collect();
    let dominant = |idxs: &[usize]| -> Option<(String, i64)> {
        let mut counts: std::collections::BTreeMap<(String, i64), usize> =
            std::collections::BTreeMap::new();
        for &i in idxs {
            let key = (
                cells[i].font_name.clone().unwrap_or_default().to_lowercase(),
                (cells[i].font_size * 10.0).round() as i64,
            );
            *counts.entry(key).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(k, _)| k)
    };
    if let (Some(a), Some(b)) = (dominant(&row0_styles), dominant(&body_styles)) {
        if a != b {
            for cell in out_cells.iter_mut().filter(|c| c.row == 0) {
                cell.is_header = true;
            }
        }
    }

    let support = hits_per_line.iter().filter(|&&h| h >= 2).count() as f64 / n_rows as f64;
    let size_bonus = if strategy == TableStrategy::Aligned {
        0.1 * (((n_rows.saturating_sub(3)) as f64) / 5.0).min(1.0)
    } else {
        0.0
    };
    let cap = if strategy == TableStrategy::Aligned { 0.9 } else { 0.95 };
    let confidence = round3((confidence_base + confidence_align_weight * support + size_bonus).min(cap));

    let x0 = columns.extents[0].0;
    let x1 = columns.extents[n_cols - 1].1;
    let y0 = run.iter().map(|l| l.y0).fold(f32::MAX, f32::min);
    let y1 = run.iter().map(|l| l.y1).fold(f32::MIN, f32::max);

    Some(TableStructure {
        bbox: Rect { x0, y0, x1, y1 },
        page: page_number,
        n_rows: n_rows as u32,
        n_cols: n_cols as u32,
        cells: out_cells,
        strategy,
        confidence,
    })
}
