//! Header-authenticated downloads for private customer exports.
use super::*;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

// The Bytes owner retains the download permit through the last socket buffer clone.
struct DownloadBody(crate::data_exports::DownloadedExport);
impl AsRef<[u8]> for DownloadBody {
    fn as_ref(&self) -> &[u8] {
        &self.0.bytes
    }
}

fn error_response(code: &str) -> axum::response::Response {
    let status = match code {
        "EXPORT_PERMISSION_DENIED" => StatusCode::FORBIDDEN,
        "EXPORT_NOT_FOUND" => StatusCode::NOT_FOUND,
        "EXPORT_EXPIRED" | "EXPORT_SOURCE_CHANGED" => StatusCode::GONE,
        "EXPORT_NOT_READY" | "EXPORT_CANCELLED" => StatusCode::CONFLICT,
        "EXPORT_BUSY" => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(serde_json::json!({"error": code}))).into_response()
}

pub(super) async fn download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    // No query-token fallback: credentials must never enter browser URLs or logs.
    let Some(token) = extract_bearer_token(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"EXPORT_AUTH_REQUIRED"})),
        )
            .into_response();
    };
    let ctx = match authenticate_jwt(&state, token) {
        Ok(ctx) => ctx,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error":"EXPORT_AUTH_REQUIRED"})),
            )
                .into_response();
        }
    };
    if let Err(response) = require_password_changed(&ctx) {
        return response;
    }
    let home = state.home_dir.clone();
    let result =
        tokio::task::spawn_blocking(move || crate::data_exports::download(&home, &ctx, &id)).await;
    let file = match result {
        Ok(Ok(file)) => file,
        Ok(Err(code)) => return error_response(&code),
        Err(_) => return error_response("EXPORT_IO"),
    };
    let content_type = file.content_type;
    let filename = file.filename.clone();
    let bytes = Bytes::from_owner(DownloadBody(file));
    let mut response = axum::response::Response::new(axum::body::Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    let disposition = format!(
        "attachment; filename*=UTF-8''{}",
        crate::files_api::encode_filename_star(&filename)
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}
