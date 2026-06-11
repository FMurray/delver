//! Round-trip tests for the persistent index (Stage A slice 1).
//!
//! Contract under test (docs/DECISIONS.md D-003): persist -> hydrate -> match
//! must behave identically to a fresh in-memory parse.
//!
//! Per D-009 the test PDF is generated in-test via lopdf (no binary fixtures),
//! and DB-backed tests skip with an explicit message when Postgres is not
//! reachable (default dev DB: postgres://delver:delver@localhost:5433/delver).

use std::collections::BTreeMap;

use delver_core::layout::MatchContext;
use delver_core::parse::{get_page_content, PageContent, PageContents};
use delver_core::search_index::PdfIndex;
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use uuid::Uuid;

use delver_store::blocking::DelverStoreBlocking;
use delver_store::{hydrate_index, DelverStore, ElementKind, SearchScope};

const HEADING_1: &str = "Management Discussion and Analysis";
const BODY_1A: &str = "Revenue grew steadily across all reporting segments during the fiscal year.";
const BODY_1B: &str = "Operating expenses stayed flat thanks to disciplined cost control programs.";
const HEADING_2: &str = "Quantitative and Qualitative Disclosures";
const BODY_2A: &str = "Interest rate exposure remains hedged through a portfolio of fixed rate swaps.";

const BBOX_EPS: f32 = 1e-4;

fn default_db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://delver:delver@localhost:5433/delver".to_string())
}

async fn connect_or_skip(test_name: &str) -> Option<DelverStore> {
    let url = default_db_url();
    match DelverStore::connect(&url).await {
        Ok(store) => Some(store),
        Err(e) => {
            eprintln!("SKIP {test_name}: Postgres unreachable at {url} ({e}); set DATABASE_URL or run scripts/dev-db.sh");
            None
        }
    }
}

fn unique_corpus(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// Build a small 2-page PDF entirely in memory:
/// page 1: one 24pt heading + two 11pt body paragraphs at known positions;
/// page 2: one 24pt heading + one 11pt body paragraph.
fn build_test_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut p1_ops = Vec::new();
    push_text_ops(&mut p1_ops, HEADING_1, 24.0, 72.0, 700.0);
    push_text_ops(&mut p1_ops, BODY_1A, 11.0, 72.0, 660.0);
    push_text_ops(&mut p1_ops, BODY_1B, 11.0, 72.0, 640.0);

    let mut p2_ops = Vec::new();
    push_text_ops(&mut p2_ops, HEADING_2, 24.0, 72.0, 700.0);
    push_text_ops(&mut p2_ops, BODY_2A, 11.0, 72.0, 660.0);

    let mut page_ids = Vec::new();
    for ops in [p1_ops, p2_ops] {
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }.encode().expect("encode content"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        page_ids.push(page_id);
    }

    let kids: Vec<Object> = page_ids.iter().map(|id| (*id).into()).collect();
    let n_pages = page_ids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => n_pages,
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf to memory");
    bytes
}

fn parse_pages(bytes: &[u8]) -> BTreeMap<u32, PageContents> {
    let doc = Document::load_mem(bytes).expect("load synthetic pdf");
    get_page_content(&doc).expect("extract page content")
}

fn assert_bbox_close(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32), ctx: &str) {
    for (i, (av, bv)) in [a.0, a.1, a.2, a.3]
        .iter()
        .zip([b.0, b.1, b.2, b.3].iter())
        .enumerate()
    {
        assert!(
            (av - bv).abs() <= BBOX_EPS,
            "{ctx}: bbox coord {i} differs: {av} vs {bv}"
        );
    }
}

/// (a) Round-trip: parse -> ingest -> load + hydrate must reproduce the
/// in-memory parse (count, ids, text, order, pages, bboxes).
#[tokio::test]
async fn roundtrip_persist_hydrate_matches_fresh_parse() {
    let Some(store) = connect_or_skip("roundtrip_persist_hydrate_matches_fresh_parse").await
    else {
        return;
    };

    let bytes = build_test_pdf();
    let pages = parse_pages(&bytes);
    let fresh = PdfIndex::new(&pages, &MatchContext::default());
    assert!(fresh.doc_len() > 0, "synthetic pdf produced no elements");

    let corpus = store
        .ensure_corpus(&unique_corpus("roundtrip"))
        .await
        .expect("ensure corpus");
    let outcome = store
        .ingest_parsed(corpus, Some("mem://synthetic.pdf"), &bytes, &pages, 1)
        .await
        .expect("ingest parsed document");
    assert!(outcome.created, "first ingest must create the document");

    let rows = store
        .load_document(outcome.document_id)
        .await
        .expect("load element rows");
    assert_eq!(rows.len(), fresh.doc_len(), "stored row count != parsed element count");

    // Rows come back in document order and carry the expected content.
    let stored_texts: Vec<&str> = rows
        .iter()
        .filter(|r| r.kind == ElementKind::Text)
        .filter_map(|r| r.text.as_deref())
        .collect();
    for expected in [HEADING_1, BODY_1A, BODY_1B, HEADING_2, BODY_2A] {
        assert!(
            stored_texts.iter().any(|t| *t == expected),
            "stored rows missing text {expected:?}"
        );
    }
    assert_eq!(rows.iter().map(|r| r.page).max(), Some(2));

    let hydrated = hydrate_index(&rows);
    assert_eq!(hydrated.doc_len(), fresh.doc_len(), "hydrated doc_len differs");

    for idx in 0..fresh.doc_len() {
        let f = fresh.content_at(idx).expect("fresh content");
        let h = hydrated.content_at(idx).expect("hydrated content");
        assert_eq!(f.id(), h.id(), "element id differs at order {idx}");
        assert_eq!(
            f.page_number(),
            h.page_number(),
            "page differs at order {idx}"
        );
        match (&f, &h) {
            (PageContent::Text(ft), PageContent::Text(ht)) => {
                assert_eq!(ft.text, ht.text, "text differs at order {idx}");
                assert!(
                    (ft.font_size - ht.font_size).abs() <= BBOX_EPS,
                    "font size differs at order {idx}"
                );
                assert_eq!(ft.font_name, ht.font_name, "font name differs at order {idx}");
                assert_bbox_close(ft.bbox, ht.bbox, &format!("text element order {idx}"));
            }
            (PageContent::Image(fi), PageContent::Image(hi)) => {
                assert_bbox_close(
                    (fi.bbox.x0, fi.bbox.y0, fi.bbox.x1, fi.bbox.y1),
                    (hi.bbox.x0, hi.bbox.y0, hi.bbox.x1, hi.bbox.y1),
                    &format!("image element order {idx}"),
                );
            }
            _ => panic!("element kind differs at order {idx}"),
        }
    }
}

/// (b) Match-equivalence: the same Levenshtein text-match run against the
/// freshly-parsed index and the hydrated index must return identical element
/// ids in identical order.
#[tokio::test]
async fn match_equivalence_fresh_vs_hydrated() {
    let Some(store) = connect_or_skip("match_equivalence_fresh_vs_hydrated").await else {
        return;
    };

    let bytes = build_test_pdf();
    let pages = parse_pages(&bytes);
    let fresh = PdfIndex::new(&pages, &MatchContext::default());

    let corpus = store
        .ensure_corpus(&unique_corpus("match-eq"))
        .await
        .expect("ensure corpus");
    let outcome = store
        .ingest_parsed(corpus, None, &bytes, &pages, 1)
        .await
        .expect("ingest parsed document");
    assert!(outcome.created);

    let rows = store
        .load_document(outcome.document_id)
        .await
        .expect("load element rows");
    let hydrated = hydrate_index(&rows);

    // Exact section-heading queries plus a fuzzy variant (Levenshtein path).
    let queries: [(&str, f64, bool); 4] = [
        (HEADING_1, 0.9, true),
        (HEADING_2, 0.9, true),
        ("Management Discussion & Analysis", 0.8, true), // fuzzy: forces non-1.0 score
        (BODY_1B, 0.9, true),
    ];

    for (query, threshold, expect_hit) in queries {
        let fresh_matches: Vec<(Uuid, f64)> = fresh
            .find_text_matches(query, threshold, None, None)
            .into_iter()
            .map(|(h, score)| (fresh.text(h).id, score))
            .collect();
        let hydrated_matches: Vec<(Uuid, f64)> = hydrated
            .find_text_matches(query, threshold, None, None)
            .into_iter()
            .map(|(h, score)| (hydrated.text(h).id, score))
            .collect();

        if expect_hit {
            assert!(
                !fresh_matches.is_empty(),
                "query {query:?} found nothing in fresh index"
            );
        }
        assert_eq!(
            fresh_matches, hydrated_matches,
            "match results diverge for query {query:?}"
        );
    }
}

/// (c) Idempotent ingest (D-008): same bytes + parse_version -> same document,
/// created=false, no extra rows. Bumping parse_version creates a new document.
/// Exercises the blocking facade.
#[test]
fn idempotent_ingest_same_bytes_same_version() {
    let url = default_db_url();
    let store = match DelverStoreBlocking::connect(&url) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("SKIP idempotent_ingest_same_bytes_same_version: Postgres unreachable at {url} ({e}); set DATABASE_URL or run scripts/dev-db.sh");
            return;
        }
    };

    let bytes = build_test_pdf();
    let corpus = store
        .ensure_corpus(&unique_corpus("idempotent"))
        .expect("ensure corpus");

    let first = store
        .ingest_document(corpus, Some("mem://synthetic.pdf"), &bytes, 1)
        .expect("first ingest");
    assert!(first.created, "first ingest must create");
    let rows_first = store
        .load_document(first.document_id)
        .expect("load after first ingest");
    assert!(!rows_first.is_empty());

    let second = store
        .ingest_document(corpus, Some("mem://synthetic.pdf"), &bytes, 1)
        .expect("second ingest");
    assert_eq!(
        first.document_id, second.document_id,
        "re-ingest must return the existing document id"
    );
    assert!(!second.created, "re-ingest must not create");

    let rows_second = store
        .load_document(second.document_id)
        .expect("load after second ingest");
    assert_eq!(
        rows_first.len(),
        rows_second.len(),
        "re-ingest must not change element row count"
    );

    // Same bytes, bumped parse_version: re-parse without losing the prior run.
    let bumped = store
        .ingest_document(corpus, Some("mem://synthetic.pdf"), &bytes, 2)
        .expect("ingest with bumped parse_version");
    assert!(bumped.created, "new parse_version must create a new document");
    assert_ne!(bumped.document_id, first.document_id);
}

/// Query API: FTS-backed text search (corpus and document scope) and the
/// GiST-backed bbox query.
#[tokio::test]
async fn text_search_and_bbox_queries() {
    let Some(store) = connect_or_skip("text_search_and_bbox_queries").await else {
        return;
    };

    let bytes = build_test_pdf();
    let corpus = store
        .ensure_corpus(&unique_corpus("queries"))
        .await
        .expect("ensure corpus");
    let outcome = store
        .ingest_document(corpus, None, &bytes, 1)
        .await
        .expect("ingest");

    let corpus_hits = store
        .text_search(SearchScope::Corpus(corpus), "revenue segments", 10)
        .await
        .expect("corpus text search");
    assert!(
        corpus_hits.iter().any(|h| h.text == BODY_1A),
        "corpus-scoped FTS should find the revenue paragraph, got {corpus_hits:?}"
    );

    let doc_hits = store
        .text_search(
            SearchScope::Document(outcome.document_id),
            "disciplined cost control",
            10,
        )
        .await
        .expect("document text search");
    assert!(
        doc_hits.iter().any(|h| h.text == BODY_1B),
        "document-scoped FTS should find the cost-control paragraph, got {doc_hits:?}"
    );
    assert!(
        doc_hits.iter().all(|h| h.document_id == outcome.document_id),
        "document scope must not leak other documents"
    );

    // Page-1 heading band in top-left coordinates: 24pt text placed at
    // pdf y=700 on a 792pt page lands around y in [74, 97].
    let band = store
        .elements_in_bbox(outcome.document_id, 1, 60.0, 60.0, 600.0, 110.0)
        .await
        .expect("bbox query");
    assert!(
        band.iter()
            .any(|r| r.text.as_deref() == Some(HEADING_1)),
        "heading band should contain the page-1 heading, got {band:?}"
    );
    assert!(
        band.iter()
            .all(|r| r.text.as_deref() != Some(BODY_1A) && r.text.as_deref() != Some(BODY_1B)),
        "body paragraphs must not intersect the heading band"
    );
    assert!(band.iter().all(|r| r.page == 1));
}
