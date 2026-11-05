//! Public customer identity for login and invitation images; no private settings.
#[cfg(test)]
#[path = "server_company_tests.rs"]
mod upload_tests;
use super::*;
use axum::http::{header, HeaderValue, StatusCode};
use axum::RequestExt;
use serde_json::json;
use crate::company_info::MAX_LOGO_BYTES;

static UPLOAD_LIMITER: std::sync::LazyLock<duduclaw_security::rate_limiter::RateLimiter> =
    std::sync::LazyLock::new(|| duduclaw_security::rate_limiter::RateLimiter::new(
        6, std::time::Duration::from_secs(60),
    ));
static UPLOAD_SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

pub(super) fn upload_route() -> axum::routing::MethodRouter<Arc<AppState>> {
    axum::routing::post(upload).layer(axum::extract::DefaultBodyLimit::max(MAX_LOGO_BYTES))
}

async fn upload_ip_admitted(limiter: &duduclaw_security::rate_limiter::RateLimiter, ip: IpAddr) -> bool {
    // Do not trust forwarding headers. Requests behind a proxy share its IP budget.
    limiter.check_and_record(&format!("ip:{}", login_throttle_ip(ip))).await
}

async fn upload_account_admitted(limiter: &duduclaw_security::rate_limiter::RateLimiter, account: &str) -> bool {
    limiter.check_and_record(&format!("user:{account}")).await
}

fn upload_error(status: StatusCode, message: &str) -> axum::response::Response {
    (status, Json(json!({"error": message}))).into_response()
}

pub(super) async fn upload(
    State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> axum::response::Response {
    // Bound unauthenticated JWT/database/audit work too. Do not amplify floods
    // with one durable audit write per throttled request.
    if !upload_ip_admitted(&UPLOAD_LIMITER, addr.ip()).await { return throttled(); }
    // Request (not Bytes/Multipart/Json) leaves the body entirely unpolled.
    // JWTs are accepted only through Authorization; cookies are never consulted.
    let (actor, mut response) = match knowledge_caller(&state, request.headers()) {
        Ok(ctx) => {
            let actor = ctx.user_id.clone();
            (actor, upload_as_admin(&state, request, ctx).await)
        }
        Err(response) => ("unauthenticated".to_owned(), response),
    };
    if !response.status().is_success() {
        crate::handlers::company_rpc::audit(&state.home_dir, &actor, "company.logo.upload", "refused");
    }
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

async fn upload_as_admin(state: &Arc<AppState>, request: axum::extract::Request, ctx: UserContext) -> axum::response::Response {
    if crate::handlers::company_rpc::current_admin(&state.home_dir, &ctx).is_err() {
        return upload_error(StatusCode::FORBIDDEN, "只有管理員可以設定公司資訊。");
    }
    if !upload_account_admitted(&UPLOAD_LIMITER, &ctx.user_id).await { return throttled(); }
    let format = match request.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        Some("image/png") => image::ImageFormat::Png,
        Some("image/jpeg") => image::ImageFormat::Jpeg,
        Some("image/webp") => image::ImageFormat::WebP,
        _ => return upload_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "請選擇 PNG、JPG 或 WebP 靜態圖片。"),
    };
    if request.headers().contains_key(header::CONTENT_ENCODING) {
        return upload_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "不支援壓縮傳輸圖片。");
    }
    if request.headers().get(header::CONTENT_LENGTH).is_some_and(|v| {
        v.to_str().ok().and_then(|s| s.parse::<u64>().ok()).is_none_or(|n| n > MAX_LOGO_BYTES as u64)
    }) {
        return upload_error(StatusCode::PAYLOAD_TOO_LARGE, "圖片不得超過 1 MiB。");
    }
    let Ok(slot) = UPLOAD_SLOTS.clone().try_acquire_owned() else { return throttled(); };
    // Apply the route's DefaultBodyLimit explicitly: Request alone does not.
    let read = tokio::time::timeout(std::time::Duration::from_secs(30), read_upload(request.into_limited_body())).await;
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(status)) => return upload_error(status, "圖片上傳失敗或超過 1 MiB。"),
        Err(_) => return upload_error(StatusCode::REQUEST_TIMEOUT, "圖片上傳逾時，請重試。"),
    };
    if image::guess_format(&bytes).ok() != Some(format) {
        return upload_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "圖片內容與格式不符。");
    }
    let home = state.home_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Retain the permit until decoding finishes even if the HTTP task is cancelled.
        let _slot = slot;
        let png = crate::company_info::sanitize_logo(&bytes).map_err(|error| (StatusCode::BAD_REQUEST, error))?;
        crate::handlers::company_rpc::current_admin(&home, &ctx)
            .map_err(|error| (StatusCode::FORBIDDEN, error))?;
        let info = crate::company_info::upload_logo(&home, &png, || crate::handlers::company_rpc::current_admin(&home, &ctx))
            .map_err(|error| (StatusCode::CONFLICT, error))?;
        // A completed write remains audited if the client disconnects mid-decode.
        crate::handlers::company_rpc::audit(&home, &ctx.user_id, "company.logo.upload", "ok");
        Ok::<_, (StatusCode, String)>(info)
    }).await;
    match result {
        Ok(Ok(info)) => Json(info).into_response(),
        Ok(Err((status, error))) => upload_error(status, &error),
        Err(_) => upload_error(StatusCode::SERVICE_UNAVAILABLE, "圖片上傳中斷，請重試。"),
    }
}

async fn read_upload(body: axum::body::Body) -> Result<Vec<u8>, StatusCode> {
    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
        // Independent hard limit: also bounds chunked bodies without Content-Length.
        if chunk.len() > MAX_LOGO_BYTES - bytes.len() { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

static LIMITER: std::sync::LazyLock<duduclaw_security::rate_limiter::RateLimiter> =
    std::sync::LazyLock::new(|| duduclaw_security::rate_limiter::RateLimiter::new(
        120, std::time::Duration::from_secs(60),
    ));

async fn admitted(addr: SocketAddr) -> bool {
    LIMITER.check_and_record(&format!("company:{}", login_throttle_ip(addr.ip()))).await
}

fn throttled() -> axum::response::Response {
    (StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "60"), (header::CACHE_CONTROL, "no-store")]).into_response()
}

pub(super) async fn company(
    State(state): State<Arc<AppState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> axum::response::Response {
    if !admitted(addr).await { return throttled(); }
    let home = state.home_dir.clone();
    let result = tokio::task::spawn_blocking(move || crate::company_info::read(&home)).await;
    let mut response = match result {
        Ok(Ok(info)) => Json(info).into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

pub(super) async fn logo(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(filename): axum::extract::Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> axum::response::Response {
    if !admitted(addr).await { return throttled(); }
    let home = state.home_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || crate::company_info::read_logo(&home, &filename)).await.ok().flatten();
    logo_response(bytes)
}

fn logo_response(bytes: Option<Vec<u8>>) -> axum::response::Response {
    let mut response = match bytes {
        Some(bytes) => ([(header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-store")]).into_response(),
    };
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn company_logo_response_is_png_nosniff_and_immutable_only_on_success() {
        let response = logo_response(Some(vec![1, 2, 3]));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(response.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
        let response = logo_response(None);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}
