//! Native GitHub REST client — issue/PR search, read, and issue comments.
//!
//! The "native tools" path for GitHub: instead of shelling out to a third-party
//! npm MCP server, we call the GitHub REST API directly and consume the access
//! token from the existing `mcp_oauth` vault (the `github` provider).
//!
//! GitHub specifics (see also `mcp_oauth::build_exchange_request`):
//! - A classic OAuth App token normally has **no expiry** (`expires_at = None`,
//!   the healthy default). If the OAuth App opts into token expiration, GitHub
//!   issues a `refresh_token`; this module refreshes in place when that applies.
//! - Every request carries `Accept: application/vnd.github+json`, a `User-Agent`
//!   (GitHub rejects requests without one), and the API version header.
//!
//! Security posture:
//! - Read tools only expand into curated structs — never the raw GitHub JSON.
//! - `github_issue_comment` is the only write and posts a **publicly visible**
//!   comment. Operators should gate it behind
//!   `agent.toml [capabilities] approval_required_tools`.

use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::mcp_oauth;

/// Provider id in the `mcp_oauth` vault.
pub const GITHUB_PROVIDER: &str = "github";

const GITHUB_BASE: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "duduclaw";
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Body truncation budget (CJK-safe codepoint count) for issue/PR bodies and
/// comment text.
const BODY_MAX_CHARS: usize = 6000;
/// Max comments returned by `github_issue_read` (most recent).
const MAX_COMMENTS: usize = 10;
/// Max files listed by `github_pr_read`.
const MAX_FILES: usize = 50;

// ── Errors ──────────────────────────────────────────────────────────────────

/// Failures obtaining a usable GitHub access token from the vault.
#[derive(Debug)]
pub enum GithubAuthError {
    NotConnected,
    /// Token expired, refresh_token present, but stored client credentials are
    /// missing → reconnect from the dashboard.
    ClientConfigMissing,
    /// Token expired and no refresh_token is available → full re-auth needed.
    NoRefreshToken,
    RefreshFailed(String),
}

impl std::fmt::Display for GithubAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubAuthError::NotConnected => write!(
                f,
                "GitHub is not connected. Open the dashboard Integrations → GitHub page and connect your GitHub account first."
            ),
            GithubAuthError::ClientConfigMissing => write!(
                f,
                "GitHub authorization expired and the stored client credentials are missing. Reconnect GitHub from the dashboard Integrations → GitHub page."
            ),
            GithubAuthError::NoRefreshToken => write!(
                f,
                "GitHub authorization expired and cannot be refreshed automatically. Reconnect GitHub from the dashboard Integrations → GitHub page."
            ),
            GithubAuthError::RefreshFailed(e) => write!(
                f,
                "Failed to refresh GitHub authorization ({e}). Reconnect GitHub from the dashboard Integrations → GitHub page."
            ),
        }
    }
}

/// Failures calling the GitHub REST API.
#[derive(Debug)]
pub enum GithubApiError {
    Auth(GithubAuthError),
    Http(String),
    /// 401 — token invalid/revoked.
    Unauthorized,
    /// 403 — insufficient scope or rate/abuse limit.
    Forbidden(String),
    /// 404 — resource not found or private without `repo` scope.
    NotFound(String),
    /// 429 / secondary rate limit after one retry.
    RateLimited,
    Api { status: u16, message: String },
}

impl std::fmt::Display for GithubApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubApiError::Auth(e) => write!(f, "{e}"),
            GithubApiError::Http(e) => write!(f, "Network error contacting GitHub: {e}"),
            GithubApiError::Unauthorized => write!(
                f,
                "GitHub rejected the request (401 Unauthorized). The authorization is no longer valid — reconnect GitHub from the dashboard Integrations → GitHub page."
            ),
            GithubApiError::Forbidden(msg) => write!(
                f,
                "GitHub denied the request (403): {msg}. This usually means the token lacks the `repo` scope for a private repository, or a rate limit was hit. Reconnect to grant `repo` if needed."
            ),
            GithubApiError::NotFound(msg) => write!(
                f,
                "GitHub could not find the resource (404): {msg}. Check the owner/repo/number, or grant `repo` scope for private repositories."
            ),
            GithubApiError::RateLimited => write!(
                f,
                "GitHub rate-limited the request. Please retry in a moment."
            ),
            GithubApiError::Api { status, message } => {
                write!(f, "GitHub API error ({status}): {message}")
            }
        }
    }
}

impl From<GithubAuthError> for GithubApiError {
    fn from(e: GithubAuthError) -> Self {
        GithubApiError::Auth(e)
    }
}

// ── Response structs (curated) ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct IssueHit {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub is_pr: bool,
    pub updated: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct IssueSearchResult {
    pub count: usize,
    pub total: u64,
    pub items: Vec<IssueHit>,
}

#[derive(Debug, Serialize)]
pub struct IssueComment {
    pub author: String,
    pub created: String,
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct IssueReadResult {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub is_pr: bool,
    pub updated: String,
    pub url: String,
    pub body: String,
    pub body_truncated: bool,
    pub comment_count: u64,
    pub comments: Vec<IssueComment>,
}

#[derive(Debug, Serialize)]
pub struct PrFile {
    pub filename: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Serialize)]
pub struct PrReadResult {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub base: String,
    pub head: String,
    pub merged: bool,
    /// GitHub computes mergeability asynchronously; `null` means "still
    /// computing" and is surfaced as `None`.
    pub mergeable: Option<bool>,
    pub updated: String,
    pub url: String,
    pub body: String,
    pub body_truncated: bool,
    pub changed_files: u64,
    pub files: Vec<PrFile>,
    pub files_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct CommentResult {
    pub id: u64,
    pub url: String,
}

// ── Token acquisition ───────────────────────────────────────────────────────

/// Return a valid GitHub access token, refreshing in place only when the token
/// actually has an expiry and a stored refresh_token (OAuth App token
/// expiration enabled). The common no-expiry case returns the stored token.
pub async fn get_valid_github_token(home_dir: &Path) -> Result<String, GithubAuthError> {
    // Fast path: non-expired (or never-expiring) token.
    if let Some(t) = mcp_oauth::get_token(home_dir, GITHUB_PROVIDER) {
        return Ok(t.access_token);
    }

    // Not returned ⇒ missing or expired.
    let existing = mcp_oauth::load_tokens(home_dir)
        .into_iter()
        .find(|t| t.provider_id == GITHUB_PROVIDER);
    let existing = match existing {
        Some(t) => t,
        None => return Err(GithubAuthError::NotConnected),
    };

    let refresh_tok = match existing.refresh_token.as_deref() {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return Err(GithubAuthError::NoRefreshToken),
    };

    let client_cfg = mcp_oauth::get_client_config(home_dir, GITHUB_PROVIDER)
        .ok_or(GithubAuthError::ClientConfigMissing)?;

    let oauth_config = mcp_oauth::McpOAuthConfig {
        provider_id: GITHUB_PROVIDER.to_string(),
        client_id: client_cfg.client_id,
        client_secret: client_cfg.client_secret,
        auth_url: client_cfg.auth_url,
        token_url: client_cfg.token_url,
        scopes: existing.scopes.clone(),
        redirect_uri: client_cfg.redirect_uri,
    };

    let mut refreshed = mcp_oauth::refresh_token(&oauth_config, &refresh_tok)
        .await
        .map_err(GithubAuthError::RefreshFailed)?;
    if refreshed.scopes.is_empty() {
        refreshed.scopes = existing.scopes.clone();
    }
    let access = refreshed.access_token.clone();
    mcp_oauth::upsert_token(home_dir, refreshed).map_err(GithubAuthError::RefreshFailed)?;
    Ok(access)
}

// ── HTTP plumbing ───────────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::Client, GithubApiError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| GithubApiError::Http(format!("client build failed: {e}")))
}

/// One GitHub REST call with a single retry on transport error / 5xx.
async fn github_request(
    token: &str,
    method: reqwest::Method,
    url: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value, GithubApiError> {
    let client = http_client()?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut req = client
            .request(method.clone(), url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", USER_AGENT)
            .header("X-GitHub-Api-Version", API_VERSION);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < 2 {
                    continue;
                }
                return Err(GithubApiError::Http(format!("request failed: {e}")));
            }
        };

        let code = resp.status().as_u16();
        if resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text)
                .map_err(|e| GithubApiError::Http(format!("invalid JSON from GitHub: {e}")));
        }

        if (500..=599).contains(&code) && attempt < 2 {
            continue;
        }

        let body_text = resp.text().await.unwrap_or_default();
        let msg = duduclaw_core::truncate_chars(&extract_api_message(&body_text), 240);
        return Err(match code {
            401 => GithubApiError::Unauthorized,
            403 | 429 => {
                // GitHub uses 403 for both scope denials and (secondary) rate
                // limits; treat the rate-limit signal as retryable-later.
                if msg.to_lowercase().contains("rate limit") {
                    GithubApiError::RateLimited
                } else {
                    GithubApiError::Forbidden(msg)
                }
            }
            404 => GithubApiError::NotFound(msg),
            _ => GithubApiError::Api { status: code, message: msg },
        });
    }
}

// ── Search ──────────────────────────────────────────────────────────────────

/// Search issues and pull requests. `query` uses GitHub search syntax
/// (`repo:owner/name is:open label:bug`). `max_results` clamped to 1..=25.
pub async fn github_search_issues(
    token: &str,
    query: &str,
    max_results: u32,
) -> Result<IssueSearchResult, GithubApiError> {
    let n = clamp(max_results, 1, 25);
    let resp = github_request(
        token,
        reqwest::Method::GET,
        &format!("{GITHUB_BASE}/search/issues"),
        &[("q", query.to_string()), ("per_page", n.to_string())],
        None,
    )
    .await?;

    let total = resp.get("total_count").and_then(|v| v.as_u64()).unwrap_or(0);
    let items: Vec<IssueHit> = resp
        .get("items")
        .and_then(|i| i.as_array())
        .map(|arr| arr.iter().map(parse_issue_hit).collect())
        .unwrap_or_default();

    Ok(IssueSearchResult {
        count: items.len(),
        total,
        items,
    })
}

// ── Issue read ──────────────────────────────────────────────────────────────

/// Read one issue plus its most recent [`MAX_COMMENTS`] comments.
pub async fn github_issue_read(
    token: &str,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<IssueReadResult, GithubApiError> {
    let issue = github_request(
        token,
        reqwest::Method::GET,
        &format!("{GITHUB_BASE}/repos/{owner}/{repo}/issues/{number}"),
        &[],
        None,
    )
    .await?;

    let body_raw = issue.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let body = duduclaw_core::truncate_chars(body_raw, BODY_MAX_CHARS);
    let body_truncated = body.chars().count() < body_raw.chars().count();
    let comment_count = issue.get("comments").and_then(|v| v.as_u64()).unwrap_or(0);

    // Fetch up to 100 comments, keep the most recent MAX_COMMENTS.
    let comments_json = github_request(
        token,
        reqwest::Method::GET,
        &format!("{GITHUB_BASE}/repos/{owner}/{repo}/issues/{number}/comments"),
        &[("per_page", "100".to_string())],
        None,
    )
    .await
    .unwrap_or(Value::Null);

    let mut comments: Vec<IssueComment> = comments_json
        .as_array()
        .map(|arr| arr.iter().map(parse_comment).collect())
        .unwrap_or_default();
    if comments.len() > MAX_COMMENTS {
        comments = comments.split_off(comments.len() - MAX_COMMENTS);
    }

    Ok(IssueReadResult {
        repo: format!("{owner}/{repo}"),
        number,
        title: issue.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        state: issue.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        author: issue
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        is_pr: issue.get("pull_request").is_some(),
        updated: issue.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        url: issue.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        body,
        body_truncated,
        comment_count,
        comments,
    })
}

// ── PR read ─────────────────────────────────────────────────────────────────

/// Read one pull request's metadata plus its changed-file list (up to
/// [`MAX_FILES`] files; diff contents are NOT fetched).
pub async fn github_pr_read(
    token: &str,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<PrReadResult, GithubApiError> {
    let pr = github_request(
        token,
        reqwest::Method::GET,
        &format!("{GITHUB_BASE}/repos/{owner}/{repo}/pulls/{number}"),
        &[],
        None,
    )
    .await?;

    let body_raw = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let body = duduclaw_core::truncate_chars(body_raw, BODY_MAX_CHARS);
    let body_truncated = body.chars().count() < body_raw.chars().count();
    let changed_files = pr.get("changed_files").and_then(|v| v.as_u64()).unwrap_or(0);

    let files_json = github_request(
        token,
        reqwest::Method::GET,
        &format!("{GITHUB_BASE}/repos/{owner}/{repo}/pulls/{number}/files"),
        &[("per_page", MAX_FILES.to_string())],
        None,
    )
    .await
    .unwrap_or(Value::Null);

    let files: Vec<PrFile> = files_json
        .as_array()
        .map(|arr| arr.iter().take(MAX_FILES).map(parse_pr_file).collect())
        .unwrap_or_default();
    let files_truncated = changed_files as usize > files.len();

    Ok(PrReadResult {
        repo: format!("{owner}/{repo}"),
        number,
        title: pr.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        state: pr.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        author: pr
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        base: pr.get("base").and_then(|b| b.get("ref")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        head: pr.get("head").and_then(|b| b.get("ref")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        merged: pr.get("merged").and_then(|v| v.as_bool()).unwrap_or(false),
        mergeable: pr.get("mergeable").and_then(|v| v.as_bool()),
        updated: pr.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        url: pr.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        body,
        body_truncated,
        changed_files,
        files,
        files_truncated,
    })
}

// ── Issue comment (write) ───────────────────────────────────────────────────

/// Post a comment on an issue or PR. **Publicly visible.**
pub async fn github_issue_comment(
    token: &str,
    owner: &str,
    repo: &str,
    number: u64,
    body: &str,
) -> Result<CommentResult, GithubApiError> {
    if body.trim().is_empty() {
        return Err(GithubApiError::Api {
            status: 400,
            message: "comment body is empty".to_string(),
        });
    }
    let payload = json!({ "body": body });
    let resp = github_request(
        token,
        reqwest::Method::POST,
        &format!("{GITHUB_BASE}/repos/{owner}/{repo}/issues/{number}/comments"),
        &[],
        Some(&payload),
    )
    .await?;

    Ok(CommentResult {
        id: resp.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
        url: resp.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

// ── Pure helpers (unit-tested) ──────────────────────────────────────────────

fn clamp(n: u32, lo: u32, hi: u32) -> u32 {
    n.max(lo).min(hi)
}

/// Extract `owner/repo` from a search result's `repository_url`
/// (`https://api.github.com/repos/{owner}/{repo}`).
fn repo_from_repository_url(url: &str) -> String {
    url.rsplit("/repos/")
        .next()
        .filter(|s| !s.is_empty() && s.contains('/'))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn parse_issue_hit(item: &Value) -> IssueHit {
    let repo_url = item
        .get("repository_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    IssueHit {
        repo: repo_from_repository_url(repo_url),
        number: item.get("number").and_then(|v| v.as_u64()).unwrap_or(0),
        title: item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        state: item.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        is_pr: item.get("pull_request").is_some(),
        updated: item.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        url: item.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    }
}

fn parse_comment(c: &Value) -> IssueComment {
    let body_raw = c.get("body").and_then(|v| v.as_str()).unwrap_or("");
    IssueComment {
        author: c
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created: c.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        body: duduclaw_core::truncate_chars(body_raw, BODY_MAX_CHARS),
    }
}

fn parse_pr_file(f: &Value) -> PrFile {
    PrFile {
        filename: f.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        status: f.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        additions: f.get("additions").and_then(|v| v.as_u64()).unwrap_or(0),
        deletions: f.get("deletions").and_then(|v| v.as_u64()).unwrap_or(0),
    }
}

/// GitHub error bodies carry a top-level `message`.
fn extract_api_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| body.to_string())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bounds() {
        assert_eq!(clamp(0, 1, 25), 1);
        assert_eq!(clamp(100, 1, 25), 25);
        assert_eq!(clamp(10, 1, 25), 10);
    }

    #[test]
    fn repo_extraction_from_repository_url() {
        assert_eq!(
            repo_from_repository_url("https://api.github.com/repos/octocat/hello-world"),
            "octocat/hello-world"
        );
        assert_eq!(repo_from_repository_url("garbage"), "");
        assert_eq!(repo_from_repository_url(""), "");
    }

    #[test]
    fn issue_hit_detects_pr() {
        let issue = json!({
            "repository_url": "https://api.github.com/repos/foo/bar",
            "number": 42,
            "title": "Bug",
            "state": "open",
            "updated_at": "2026-07-26T00:00:00Z",
            "html_url": "https://github.com/foo/bar/issues/42"
        });
        let h = parse_issue_hit(&issue);
        assert_eq!(h.repo, "foo/bar");
        assert_eq!(h.number, 42);
        assert!(!h.is_pr);

        let pr = json!({
            "repository_url": "https://api.github.com/repos/foo/bar",
            "number": 7,
            "title": "Feature",
            "state": "open",
            "pull_request": { "url": "https://api.github.com/repos/foo/bar/pulls/7" },
            "html_url": "https://github.com/foo/bar/pull/7"
        });
        assert!(parse_issue_hit(&pr).is_pr);
    }

    #[test]
    fn comment_parsing() {
        let c = json!({
            "user": {"login": "alice"},
            "created_at": "2026-07-26T01:00:00Z",
            "body": "looks good"
        });
        let parsed = parse_comment(&c);
        assert_eq!(parsed.author, "alice");
        assert_eq!(parsed.body, "looks good");
    }

    #[test]
    fn pr_file_parsing() {
        let f = json!({
            "filename": "src/main.rs",
            "status": "modified",
            "additions": 12,
            "deletions": 3
        });
        let pf = parse_pr_file(&f);
        assert_eq!(pf.filename, "src/main.rs");
        assert_eq!(pf.additions, 12);
        assert_eq!(pf.deletions, 3);
    }

    #[test]
    fn api_message_extraction() {
        let body = r#"{"message":"Not Found","documentation_url":"https://docs.github.com"}"#;
        assert_eq!(extract_api_message(body), "Not Found");
        assert_eq!(extract_api_message("boom"), "boom");
    }
}
