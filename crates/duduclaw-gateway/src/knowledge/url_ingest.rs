//! Web pages added by URL. Fetched when the ingest job runs (never cached),
//! through the same SSRF policy as the other public-page readers:
//! `web_fetch::validate_workflow_public_url` (http/https only, no userinfo,
//! no internal hosts), DNS pinned to the validated public addresses, no
//! redirects, 5 MB ceiling. Only HTML and plain text are accepted.

use duduclaw_knowledge::extract::ExtractError;

use crate::web_fetch::{FetchError, validate_workflow_public_url, web_fetch_workflow};

/// Longest URL accepted.
pub const MAX_URL_LEN: usize = 2048;

fn blocked() -> ExtractError {
    ExtractError::new(
        "url_blocked",
        "這個網址無法加入：只接受公開的 http／https 網頁，不能是內部網路位址或帶帳號密碼的網址。",
    )
}

/// Check a URL when it is added (fast refusal before a job is queued).
pub fn validate(url: &str) -> Result<String, ExtractError> {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_URL_LEN || url.chars().any(char::is_control) {
        return Err(blocked());
    }
    validate_workflow_public_url(url)
        .map(|u| u.to_string())
        .map_err(|_| blocked())
}

fn accepted_type(content_type: &str) -> bool {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    ct.is_empty() || ct == "text/html" || ct == "application/xhtml+xml" || ct == "text/plain"
}

/// Fetch the page body.
pub async fn fetch(url: &str) -> Result<Vec<u8>, ExtractError> {
    let url = validate(url)?;
    match web_fetch_workflow(&url).await {
        Ok(r) if !accepted_type(&r.content_type) => Err(ExtractError::new(
            "url_fetch_failed",
            "這個網址不是網頁（HTML 或純文字），無法加入。PDF 等檔案請下載後用上傳的方式加入。",
        )),
        Ok(r) => Ok(r.body.into_bytes()),
        Err(FetchError::SsrfBlocked(_)) => Err(blocked()),
        Err(FetchError::TooLarge) => Err(ExtractError::new("url_too_large", "網頁內容超過 5 MB 上限。")),
        Err(e) => {
            tracing::info!(error = %e, "knowledge url fetch failed");
            Err(ExtractError::new(
                "url_fetch_failed",
                "無法取得這個網頁，請確認網址正確且網站可以公開瀏覽。",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_and_credentialed_urls_are_blocked() {
        for u in [
            "http://localhost/x",
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://10.0.0.1/",
            "http://user:pass@example.com/",
            "file:///etc/passwd",
            "ftp://example.com/",
            "",
        ] {
            assert_eq!(validate(u).unwrap_err().code, "url_blocked", "{u}");
        }
        assert!(validate("https://example.com/faq").is_ok());
    }

    #[test]
    fn content_types() {
        assert!(accepted_type("text/html; charset=utf-8"));
        assert!(accepted_type("text/plain"));
        assert!(!accepted_type("application/pdf"));
        assert!(!accepted_type("image/png"));
    }
}
