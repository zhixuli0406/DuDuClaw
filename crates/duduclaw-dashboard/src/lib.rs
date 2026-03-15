use axum::{
    Router,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    extract::Path,
    routing::get,
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "dist/"]
struct Asset;

/// Returns the Axum router for serving the dashboard SPA.
pub fn dashboard_router() -> Router {
    Router::new()
        .route("/assets/{*path}", get(static_handler))
        .fallback(get(index_handler))
}

async fn index_handler() -> impl IntoResponse {
    match Asset::get("index.html") {
        Some(content) => Html(
            std::str::from_utf8(content.data.as_ref())
                .unwrap_or("")
                .to_string(),
        )
        .into_response(),
        None => (StatusCode::NOT_FOUND, "Dashboard not built").into_response(),
    }
}

async fn static_handler(Path(path): Path<String>) -> impl IntoResponse {
    let path = format!("assets/{}", path);
    match Asset::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
                .body(axum::body::Body::from(content.data.to_vec()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from("Not found"))
            .unwrap(),
    }
}
