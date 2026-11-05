//! Small, bounded HTTP helpers for the OAuth and registry calls.
//!
//! Every request goes through [`super::url_policy::pinned_client`] (checked
//! URL, screened and pinned addresses, no automatic redirects) and every body
//! is read with a byte cap, so a hostile server can neither redirect the
//! gateway to a private address nor make it buffer an unbounded answer.

use std::time::Duration;

use serde_json::Value;
use url::Url;

use super::url_policy::{OutboundPolicy, check_outbound, pinned_client};

/// Default body cap for metadata / token answers.
pub const MAX_JSON_BODY: usize = 1024 * 1024;
/// Default timeout for one OAuth / metadata request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Redirects followed for a metadata GET (each hop re-checked and re-pinned).
const MAX_REDIRECTS: usize = 3;

/// Read a response body, refusing more than `max` bytes.
pub async fn read_capped(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if let Some(len) = resp.content_length()
        && len as usize > max
    {
        return Err(format!("response too large ({len} bytes, max {max})"));
    }
    let mut out: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| "reading the response failed".to_string())?
    {
        if out.len() + chunk.len() > max {
            return Err(format!("response too large (over {max} bytes)"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Parse a capped body as JSON (an empty body is `Value::Null`).
fn body_json(bytes: &[u8]) -> Value {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Value::Null;
    }
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

/// GET a JSON document. Follows up to three redirects, re-checking and
/// re-pinning each hop under the same policy. Returns status and JSON body
/// (`Null` when the body is not JSON).
pub async fn get_json(
    url: &Url,
    policy: OutboundPolicy,
    max: usize,
) -> Result<(u16, Value), String> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        check_outbound(&current, policy)?;
        let client = pinned_client(&current, policy, REQUEST_TIMEOUT).await?;
        let resp = client
            .get(current.clone())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| format!("request to {} failed", host_label(&current)))?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "redirect without a Location header".to_string())?;
            current = current
                .join(loc)
                .map_err(|e| format!("invalid redirect target: {e}"))?;
            continue;
        }
        let bytes = read_capped(resp, max).await?;
        return Ok((status.as_u16(), body_json(&bytes)));
    }
    Err("too many redirects".into())
}

/// POST a form (OAuth token endpoint). Never follows redirects.
pub async fn post_form(
    url: &Url,
    policy: OutboundPolicy,
    form: &[(&str, &str)],
    basic_auth: Option<(&str, &str)>,
) -> Result<(u16, Value), String> {
    let client = pinned_client(url, policy, REQUEST_TIMEOUT).await?;
    let mut req = client
        .post(url.clone())
        .header("Accept", "application/json")
        .form(form);
    if let Some((user, pass)) = basic_auth {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req
        .send()
        .await
        .map_err(|_| format!("request to {} failed", host_label(url)))?;
    let status = resp.status().as_u16();
    let bytes = read_capped(resp, MAX_JSON_BODY).await?;
    Ok((status, body_json(&bytes)))
}

/// POST a JSON body (dynamic client registration). Never follows redirects.
pub async fn post_json(url: &Url, policy: OutboundPolicy, body: &Value) -> Result<(u16, Value), String> {
    let client = pinned_client(url, policy, REQUEST_TIMEOUT).await?;
    let resp = client
        .post(url.clone())
        .header("Accept", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|_| format!("request to {} failed", host_label(url)))?;
    let status = resp.status().as_u16();
    let bytes = read_capped(resp, MAX_JSON_BODY).await?;
    Ok((status, body_json(&bytes)))
}

/// `host[:port]` of a URL, for messages (never the path or query, which can
/// carry a key).
pub fn host_label(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => "(no host)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_label_never_shows_path_or_query() {
        let u = Url::parse("https://mcp.example.com:8443/api/s/SECRET/mcp?k=v").unwrap();
        assert_eq!(host_label(&u), "mcp.example.com:8443");
    }

    #[tokio::test]
    async fn failed_requests_never_expose_secret_url_paths() {
        // Keep the port bound but not listening: connect fails without a port race.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let url = Url::parse(&format!("http://{}/api/SECRET_PATH?key=SECRET_QUERY", socket.local_addr().unwrap())).unwrap();
        let policy = OutboundPolicy::for_mcp_url(&url);
        let errors = [
            get_json(&url, policy, MAX_JSON_BODY).await.unwrap_err(),
            post_form(&url, policy, &[], None).await.unwrap_err(),
            post_json(&url, policy, &serde_json::json!({})).await.unwrap_err(),
        ];
        for error in errors {
            assert_eq!(error, format!("request to {} failed", host_label(&url)));
            assert!(!error.contains("SECRET"));
            assert!(!error.contains("/api/"));
        }
    }
}
