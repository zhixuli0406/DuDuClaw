use axum::{
    Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
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

/// Common security headers for all dashboard responses (MW-H7).
fn security_headers(builder: axum::http::response::Builder) -> axum::http::response::Builder {
    builder
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Frame-Options", "DENY")
        .header("X-XSS-Protection", "1; mode=block")
        .header("Referrer-Policy", "strict-origin-when-cross-origin")
        .header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; img-src 'self' data:;",
        )
}

async fn index_handler() -> impl IntoResponse {
    match Asset::get("index.html") {
        Some(content) => {
            let html = std::str::from_utf8(content.data.as_ref())
                .unwrap_or("")
                .to_string();
            security_headers(Response::builder())
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(axum::body::Body::from(html))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from("Dashboard not built"))
            .unwrap(),
    }
}

async fn static_handler(Path(path): Path<String>) -> impl IntoResponse {
    let path = format!("assets/{}", path);
    match Asset::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            security_headers(Response::builder())
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
