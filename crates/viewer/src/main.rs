//! Viewer server: Leptos SSR app plus a small plain-JSON REST surface
//! (`/api/v/...`) over the same service layer, so the store can be driven
//! with curl and page images load as ordinary `<img>` requests (DV-004).

#[cfg(feature = "ssr")]
mod rest {
    use axum::extract::{Path, Query};
    use axum::http::{header, StatusCode};
    use axum::response::{IntoResponse, Response};
    use bytes::Bytes;
    use std::collections::HashMap;
    use viewer::store;

    fn json_error(status: StatusCode, message: String) -> Response {
        let body = serde_json::json!({ "error": message }).to_string();
        (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response()
    }

    fn json_ok<T: serde::Serialize>(value: &T) -> Response {
        match serde_json::to_string(value) {
            Ok(body) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
            Err(e) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serializing response: {e}"),
            ),
        }
    }

    /// GET /api/v/docs — all documents joined with their corpus.
    pub async fn list_documents() -> Response {
        match store::list_documents().await {
            Ok(docs) => json_ok(&docs),
            Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
        }
    }

    /// GET /api/v/docs/{doc_id} — one document summary.
    pub async fn document(Path(doc_id): Path<String>) -> Response {
        match store::document_summary(&doc_id).await {
            Ok(Some(doc)) => json_ok(&doc),
            Ok(None) => json_error(
                StatusCode::NOT_FOUND,
                format!("unknown document {doc_id}"),
            ),
            Err(e) => json_error(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }

    /// GET /api/v/docs/{doc_id}/pages/{page}/elements — overlay JSON
    /// (page is the 0-based viewer index).
    pub async fn page_elements(Path((doc_id, page)): Path<(String, usize)>) -> Response {
        match store::page_elements(&doc_id, page).await {
            Ok(elements) => json_ok(&elements),
            Err(e) => json_error(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }

    /// GET /api/v/docs/{doc_id}/pages/{page}/image.webp — on-demand raster
    /// from the byte-cache; 404 with a reason when the source is missing.
    pub async fn page_image(Path((doc_id, page)): Path<(String, usize)>) -> Response {
        match store::page_raster(&doc_id, page).await {
            Ok(store::PageRaster::Rendered { webp, .. }) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "image/webp"),
                    (header::CACHE_CONTROL, "private, max-age=300"),
                ],
                webp,
            )
                .into_response(),
            Ok(store::PageRaster::Unavailable { reason }) => {
                json_error(StatusCode::NOT_FOUND, reason)
            }
            Err(e) => json_error(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }

    /// GET /api/v/docs/{doc_id}/pages/{page}/meta — raster layout metadata.
    pub async fn page_meta(Path((doc_id, page)): Path<(String, usize)>) -> Response {
        match store::page_raster(&doc_id, page).await {
            Ok(raster) => json_ok(&raster.meta()),
            Err(e) => json_error(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }

    /// GET /api/v/docs/{doc_id}/palette — heading candidates + detected
    /// tables for the doc-aware query palette (DV-012).
    pub async fn palette(Path(doc_id): Path<String>) -> Response {
        match store::doc_palette(&doc_id).await {
            Ok(palette) => json_ok(&palette),
            Err(e) => json_error(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }

    /// POST /api/v/docs/{doc_id}/template — body is DocQL source; returns the
    /// outputs JSON, or 422 with a readable error (fail-loud matchers, D-006).
    pub async fn run_template(Path(doc_id): Path<String>, template: String) -> Response {
        match store::execute_template(&doc_id, &template).await {
            Ok(output) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                output,
            )
                .into_response(),
            Err(e) => json_error(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")),
        }
    }

    /// POST /api/v/upload?corpus=viewer-dev&filename=doc.pdf — body is the
    /// raw PDF bytes; ingests into the store + byte-cache (DV-002).
    pub async fn upload(Query(params): Query<HashMap<String, String>>, body: Bytes) -> Response {
        let filename = params
            .get("filename")
            .cloned()
            .unwrap_or_else(|| "upload.pdf".to_string());
        let corpus = params.get("corpus").map(String::as_str);
        match store::ingest_upload(&filename, corpus, body.to_vec()).await {
            Ok(receipt) => json_ok(&receipt),
            Err(e) => json_error(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")),
        }
    }
}

#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post};
    use axum::Router;
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use viewer::app::*;

    let conf = get_configuration(None).unwrap();
    let addr = conf.leptos_options.site_addr;
    let leptos_options = conf.leptos_options;
    // Generate the list of routes in your Leptos App
    let routes = generate_route_list(App);

    let app = Router::new()
        .route("/api/v/docs", get(rest::list_documents))
        .route("/api/v/docs/{doc_id}", get(rest::document))
        .route(
            "/api/v/docs/{doc_id}/pages/{page}/elements",
            get(rest::page_elements),
        )
        .route(
            "/api/v/docs/{doc_id}/pages/{page}/image.webp",
            get(rest::page_image),
        )
        .route(
            "/api/v/docs/{doc_id}/pages/{page}/meta",
            get(rest::page_meta),
        )
        .route("/api/v/docs/{doc_id}/palette", get(rest::palette))
        .route("/api/v/docs/{doc_id}/template", post(rest::run_template))
        .route("/api/v/upload", post(rest::upload))
        .leptos_routes(&leptos_options, routes, {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(leptos_options);

    println!("viewer listening on http://{addr} (db: {})", viewer::store::db_url());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

#[cfg(not(feature = "ssr"))]
pub fn main() {
    // no client-side main function
}
