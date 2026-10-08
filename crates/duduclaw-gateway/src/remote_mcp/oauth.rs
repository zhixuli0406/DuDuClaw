//! OAuth 2.1 client for remote MCP servers, per the MCP authorization spec.
//!
//! 1. **Protected resource metadata** (RFC 9728): the MCP server answers an
//!    unauthenticated request with `401` and
//!    `WWW-Authenticate: Bearer resource_metadata="…"`; without that parameter
//!    the well-known URIs are tried (path-inserted, then root).
//! 2. **Authorization server metadata** (RFC 8414, then OpenID Connect
//!    discovery), with the `issuer` checked against the server it was fetched
//!    for and `S256` required in `code_challenge_methods_supported` (the spec
//!    says a client must refuse when PKCE support is not advertised).
//! 3. **Dynamic client registration** (RFC 7591), `client_name` "DuDuClaw",
//!    public client (`token_endpoint_auth_method = none`) unless the server
//!    insists on a secret. An operator may instead supply a pre-registered
//!    client id/secret. Client ID Metadata Documents (CIMD) are not
//!    implemented: they need a publicly reachable `client.json` URL, which a
//!    self-hosted gateway does not have.
//! 4. **Authorization code + PKCE S256** with the RFC 8707 `resource`
//!    parameter on the authorize, token and refresh requests.
//! 5. **Refresh with rotation**: a refresh answer's new refresh token replaces
//!    the old one; an answer without one keeps the old one.
//!
//! Every URL passes [`super::url_policy`]; nothing here follows a redirect on
//! a POST.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use super::http::{MAX_JSON_BODY, get_json, oauth_error_text, post_form, post_json};
use super::url_policy::{OutboundPolicy, check_outbound};

/// Parameters of a `WWW-Authenticate: Bearer …` challenge that matter here.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// Parse a `WWW-Authenticate` header value (RFC 7235 auth-params). Only the
/// `Bearer` scheme is read; quoted and token values are both accepted.
pub fn parse_www_authenticate(header: &str) -> Option<BearerChallenge> {
    let trimmed = header.trim_start();
    // `get` (not `[..6]`): a multi-byte character there must not panic.
    let rest = match (trimmed.get(..6), trimmed.get(6..)) {
        (Some(scheme), Some(rest)) if scheme.eq_ignore_ascii_case("bearer") => rest,
        _ => return None,
    };
    if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with(',')) {
        return None;
    }
    let mut out = BearerChallenge::default();
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == ',') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && chars[i] != ',' && chars[i] != ' ' {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
        if i >= chars.len() || chars[i] != '=' {
            // A new scheme (e.g. `, Basic realm=…`) or a bare token: stop at a
            // following scheme name, skip a bare token.
            continue;
        }
        i += 1; // '='
        let mut value = String::new();
        if i < chars.len() && chars[i] == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                value.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote
        } else {
            while i < chars.len() && chars[i] != ',' && chars[i] != ' ' {
                value.push(chars[i]);
                i += 1;
            }
        }
        match key.as_str() {
            "resource_metadata" => out.resource_metadata = Some(value),
            "scope" => out.scope = Some(value),
            "error" => out.error = Some(value),
            _ => {}
        }
    }
    Some(out)
}

/// RFC 9728 §3.1 well-known URIs for an MCP URL: path-inserted first
/// (`/.well-known/oauth-protected-resource/<path>`), then the root.
pub fn protected_resource_metadata_urls(mcp_url: &Url) -> Vec<Url> {
    let mut out = Vec::new();
    let path = mcp_url.path().trim_end_matches('/');
    let mut origin = mcp_url.clone();
    origin.set_query(None);
    origin.set_fragment(None);
    if !path.is_empty() {
        let mut u = origin.clone();
        u.set_path(&format!("/.well-known/oauth-protected-resource{path}"));
        out.push(u);
    }
    let mut root = origin;
    root.set_path("/.well-known/oauth-protected-resource");
    out.push(root);
    out
}

/// Metadata URIs to try for an authorization server issuer (MCP spec order):
/// with a path component, RFC 8414 path-inserted, OIDC path-inserted, OIDC
/// path-appended; without, RFC 8414 then OIDC at the root.
pub fn authorization_server_metadata_urls(issuer: &Url) -> Vec<Url> {
    let path = issuer.path().trim_end_matches('/');
    let mut base = issuer.clone();
    base.set_query(None);
    base.set_fragment(None);
    let with = |p: String| {
        let mut u = base.clone();
        u.set_path(&p);
        u
    };
    if path.is_empty() {
        vec![
            with("/.well-known/oauth-authorization-server".into()),
            with("/.well-known/openid-configuration".into()),
        ]
    } else {
        vec![
            with(format!("/.well-known/oauth-authorization-server{path}")),
            with(format!("/.well-known/openid-configuration{path}")),
            with(format!("{path}/.well-known/openid-configuration")),
        ]
    }
}

/// The canonical resource identifier for an MCP URL (RFC 8707 / MCP spec):
/// scheme, lowercase host, port, path without a trailing slash; no query or
/// fragment.
pub fn canonical_resource(mcp_url: &Url) -> String {
    let mut u = mcp_url.clone();
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().trim_end_matches('/').to_string();
    u.set_path(&path);
    let s = u.to_string();
    s.trim_end_matches('/').to_string()
}

/// Authorization server metadata, the fields used here.
#[derive(Debug, Clone)]
pub struct AuthServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub registration_endpoint: Option<Url>,
    pub token_endpoint_auth_methods: Vec<String>,
    /// RFC 7009 revocation endpoint, when advertised (screened like the
    /// other endpoints).
    pub revocation_endpoint: Option<Url>,
}

/// The result of discovery for one MCP URL.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub metadata: AuthServerMetadata,
    /// Resource indicator to send (from the protected resource metadata, or
    /// the canonical MCP URL).
    pub resource: String,
    /// Scope from the challenge or the resource metadata, if any.
    pub scope: Option<String>,
}

fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn endpoint(v: &Value, key: &str, policy: OutboundPolicy) -> Result<Option<Url>, String> {
    let Some(raw) = v.get(key).and_then(|x| x.as_str()) else {
        return Ok(None);
    };
    let u = Url::parse(raw).map_err(|e| format!("{key} is not a valid URL: {e}"))?;
    check_outbound(&u, policy).map_err(|e| format!("{key} refused: {e}"))?;
    Ok(Some(u))
}

/// Parse and validate an authorization server metadata document fetched for
/// `issuer`. Pure.
pub fn parse_as_metadata(
    doc: &Value,
    issuer: &Url,
    policy: OutboundPolicy,
) -> Result<AuthServerMetadata, String> {
    let declared = doc
        .get("issuer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "authorization server metadata has no issuer".to_string())?;
    let norm = |s: &str| s.trim_end_matches('/').to_string();
    if norm(declared) != norm(issuer.as_str()) {
        return Err(format!(
            "authorization server metadata names a different issuer ({}), refusing",
            duduclaw_core::truncate_chars(declared, 120)
        ));
    }
    let methods = string_list(doc, "code_challenge_methods_supported");
    if !methods.iter().any(|m| m == "S256") {
        return Err("the authorization server does not advertise PKCE S256 support; refusing (MCP spec)".into());
    }
    let authorization_endpoint = endpoint(doc, "authorization_endpoint", policy)?
        .ok_or_else(|| "authorization server metadata has no authorization_endpoint".to_string())?;
    let token_endpoint = endpoint(doc, "token_endpoint", policy)?
        .ok_or_else(|| "authorization server metadata has no token_endpoint".to_string())?;
    let registration_endpoint = endpoint(doc, "registration_endpoint", policy)?;
    // A revocation endpoint that fails the screen is dropped, not fatal:
    // revocation is best effort and never needed to sign in.
    let revocation_endpoint = endpoint(doc, "revocation_endpoint", policy).ok().flatten();
    Ok(AuthServerMetadata {
        revocation_endpoint,
        issuer: declared.to_string(),
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        token_endpoint_auth_methods: string_list(doc, "token_endpoint_auth_methods_supported"),
    })
}

/// Probe the MCP URL without a token and read the `WWW-Authenticate`
/// challenge, if any. Returns `(status, challenge)`.
pub async fn probe_challenge(
    mcp_url: &Url,
    policy: OutboundPolicy,
) -> Result<(u16, Option<BearerChallenge>), String> {
    let client =
        super::url_policy::pinned_client(mcp_url, policy, super::http::REQUEST_TIMEOUT).await?;
    let resp = client
        .post(mcp_url.clone())
        .header("Accept", "application/json, text/event-stream")
        .json(&initialize_probe_frame())
        .send()
        .await
        .map_err(|e| format!("cannot reach {}: {e}", super::http::host_label(mcp_url)))?;
    let status = resp.status().as_u16();
    let challenge = resp
        .headers()
        .get_all("www-authenticate")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(parse_www_authenticate);
    Ok((status, challenge))
}

/// The `initialize` request used to probe a server.
pub fn initialize_probe_frame() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": super::bridge::DEFAULT_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "DuDuClaw", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

/// Full discovery for an MCP URL.
pub async fn discover(mcp_url: &Url, policy: OutboundPolicy) -> Result<Discovery, String> {
    let (_, challenge) = probe_challenge(mcp_url, policy).await?;
    let challenge = challenge.unwrap_or_default();

    // Protected resource metadata.
    let mut prm_urls: Vec<Url> = Vec::new();
    if let Some(raw) = &challenge.resource_metadata {
        let u = Url::parse(raw).map_err(|e| format!("resource_metadata is not a valid URL: {e}"))?;
        prm_urls.push(u);
    }
    prm_urls.extend(protected_resource_metadata_urls(mcp_url));

    let mut prm: Option<Value> = None;
    let mut tried: Vec<String> = Vec::new();
    for u in &prm_urls {
        if let Err(e) = check_outbound(u, policy) {
            tried.push(e);
            continue;
        }
        match get_json(u, policy, MAX_JSON_BODY).await {
            Ok((200, doc)) if doc.is_object() => {
                prm = Some(doc);
                break;
            }
            Ok((status, _)) => tried.push(format!("HTTP {status}")),
            Err(e) => tried.push(e),
        }
    }

    let expected_resource = canonical_resource(mcp_url);
    let (as_url, resource, prm_scope) = match &prm {
        Some(doc) => {
            let servers = string_list(doc, "authorization_servers");
            let first = servers
                .first()
                .ok_or_else(|| "protected resource metadata lists no authorization server".to_string())?;
            let as_url = Url::parse(first)
                .map_err(|e| format!("authorization server URL is invalid: {e}"))?;
            // RFC 9728 §3.3: the metadata must be about this resource.
            let resource = match doc.get("resource").and_then(|v| v.as_str()) {
                Some(r) => {
                    let ru = Url::parse(r).map_err(|e| format!("resource is not a URL: {e}"))?;
                    if ru.origin() != mcp_url.origin() {
                        return Err("protected resource metadata is for a different server; refusing".into());
                    }
                    r.trim_end_matches('/').to_string()
                }
                None => expected_resource.clone(),
            };
            let scope = {
                let s = string_list(doc, "scopes_supported");
                if s.is_empty() { None } else { Some(s.join(" ")) }
            };
            (as_url, resource, scope)
        }
        // Older servers (2025-03-26 spec) host the authorization server at
        // the MCP origin.
        None => {
            let mut origin = mcp_url.clone();
            origin.set_path("/");
            origin.set_query(None);
            (origin, expected_resource.clone(), None)
        }
    };
    check_outbound(&as_url, policy).map_err(|e| format!("authorization server refused: {e}"))?;

    let mut metadata: Option<AuthServerMetadata> = None;
    let mut last_err = String::new();
    for u in authorization_server_metadata_urls(&as_url) {
        match get_json(&u, policy, MAX_JSON_BODY).await {
            Ok((200, doc)) if doc.is_object() => {
                metadata = Some(parse_as_metadata(&doc, &as_url, policy)?);
                break;
            }
            Ok((status, _)) => last_err = format!("HTTP {status}"),
            Err(e) => last_err = e,
        }
    }
    let metadata = metadata.ok_or_else(|| {
        format!(
            "could not find the authorization server's metadata ({last_err}){}",
            if prm.is_none() && !tried.is_empty() {
                format!("; protected resource metadata: {}", duduclaw_core::truncate_chars(&tried.join(" | "), 300))
            } else {
                String::new()
            }
        )
    })?;
    Ok(Discovery {
        metadata,
        resource,
        scope: challenge.scope.or(prm_scope),
    })
}

/// Client credentials from registration or the operator.
#[derive(Clone)]
pub struct ClientCredentials {
    pub client_id: String,
    pub client_secret: Option<String>,
    /// `none`, `client_secret_post` or `client_secret_basic`.
    pub token_endpoint_auth: String,
}

/// The RFC 7591 registration request body. Pure.
pub fn registration_body(redirect_uri: &str, scope: Option<&str>) -> Value {
    let mut body = json!({
        "client_name": "DuDuClaw",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(s) = scope {
        body["scope"] = json!(s);
    }
    body
}

/// Register DuDuClaw as a client (RFC 7591).
pub async fn register_client(
    meta: &AuthServerMetadata,
    redirect_uri: &str,
    scope: Option<&str>,
    policy: OutboundPolicy,
) -> Result<ClientCredentials, String> {
    let reg = meta.registration_endpoint.as_ref().ok_or_else(|| {
        "the authorization server does not support dynamic client registration; \
         register a client for DuDuClaw there and enter its client id"
            .to_string()
    })?;
    let (status, body) = post_json(reg, policy, &registration_body(redirect_uri, scope)).await?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "client registration failed (HTTP {status}): {}",
            oauth_error_text(&body)
        ));
    }
    let client_id = body
        .get("client_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "client registration answer has no client_id".to_string())?
        .to_string();
    let client_secret = body
        .get("client_secret")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let token_endpoint_auth = match (&client_secret, body.get("token_endpoint_auth_method").and_then(|v| v.as_str())) {
        (None, _) => "none".to_string(),
        (Some(_), Some("client_secret_basic")) => "client_secret_basic".to_string(),
        (Some(_), _) => "client_secret_post".to_string(),
    };
    Ok(ClientCredentials { client_id, client_secret, token_endpoint_auth })
}

/// How to authenticate operator-supplied credentials at the token endpoint.
pub fn auth_method_for_supplied(meta: &AuthServerMetadata, has_secret: bool) -> String {
    if !has_secret {
        return "none".into();
    }
    if meta.token_endpoint_auth_methods.iter().any(|m| m == "client_secret_post")
        || meta.token_endpoint_auth_methods.is_empty()
    {
        "client_secret_post".into()
    } else {
        "client_secret_basic".into()
    }
}

/// `(code_verifier, code_challenge)`: 32 random bytes from the OS RNG,
/// base64url (43 characters), challenge = base64url(SHA-256(verifier)).
pub fn pkce_pair() -> (String, String) {
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    let verifier = URL_SAFE_NO_PAD.encode(buf);
    let challenge = pkce_challenge(&verifier);
    (verifier, challenge)
}

/// S256 challenge for a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// An unguessable `state` value (32 random bytes, base64url).
pub fn random_state() -> String {
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Build the authorization request URL. Pure.
pub fn build_authorize_url(
    meta: &AuthServerMetadata,
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
    resource: &str,
    scope: Option<&str>,
) -> Url {
    let mut u = meta.authorization_endpoint.clone();
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("code_challenge", code_challenge);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("state", state);
        q.append_pair("resource", resource);
        if let Some(s) = scope.filter(|s| !s.trim().is_empty()) {
            q.append_pair("scope", s);
        }
    }
    u
}

/// Tokens from a token endpoint answer.
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: Option<i64>,
    pub scope: Option<String>,
}

/// Parse a token endpoint answer. `now` is Unix seconds. Pure.
pub fn parse_token_response(status: u16, body: &Value, now: i64) -> Result<TokenSet, TokenError> {
    if !(200..300).contains(&status) {
        let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let text = format!("token request failed (HTTP {status}): {}", oauth_error_text(body));
        return Err(if code == "invalid_grant" {
            TokenError::InvalidGrant(text)
        } else {
            TokenError::Other(text)
        });
    }
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| TokenError::Other("token answer has no access_token".into()))?
        .to_string();
    if let Some(tt) = body.get("token_type").and_then(|v| v.as_str())
        && !tt.eq_ignore_ascii_case("bearer")
    {
        return Err(TokenError::Other(format!(
            "unsupported token_type {}",
            duduclaw_core::truncate_chars(tt, 40)
        )));
    }
    let expires_at = body
        .get("expires_in")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .filter(|s| *s > 0)
        .map(|s| now.saturating_add(s));
    Ok(TokenSet {
        access_token,
        refresh_token: body
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        expires_at,
        scope: body.get("scope").and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// Why a token request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// `invalid_grant`: the code or refresh token is no longer valid.
    InvalidGrant(String),
    Other(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGrant(s) | Self::Other(s) => f.write_str(s),
        }
    }
}

/// RFC 7009 token revocation, best effort. Client authentication follows
/// the token endpoint's method. Returns the HTTP status (200 = revoked or
/// already invalid, per RFC 7009 §2.2).
pub async fn revoke_token(
    endpoint: &Url,
    policy: OutboundPolicy,
    creds: &ClientCredentials,
    token: &str,
    token_type_hint: &str,
) -> Result<u16, String> {
    let mut form: Vec<(&str, String)> = vec![("token", token.to_string()), ("token_type_hint", token_type_hint.to_string())];
    let mut basic: Option<(&str, &str)> = None;
    match (creds.token_endpoint_auth.as_str(), creds.client_secret.as_deref()) {
        ("client_secret_basic", Some(secret)) => basic = Some((creds.client_id.as_str(), secret)),
        ("client_secret_post", Some(secret)) => {
            form.push(("client_id", creds.client_id.clone()));
            form.push(("client_secret", secret.to_string()));
        }
        _ => form.push(("client_id", creds.client_id.clone())),
    }
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
    super::http::post_form(endpoint, policy, &pairs, basic).await.map(|(status, _)| status)
}

async fn token_request(
    token_endpoint: &Url,
    policy: OutboundPolicy,
    creds: &ClientCredentials,
    mut form: Vec<(&str, String)>,
) -> Result<TokenSet, TokenError> {
    let mut basic: Option<(&str, &str)> = None;
    match (creds.token_endpoint_auth.as_str(), creds.client_secret.as_deref()) {
        ("client_secret_basic", Some(secret)) => basic = Some((creds.client_id.as_str(), secret)),
        ("client_secret_post", Some(secret)) => {
            form.push(("client_id", creds.client_id.clone()));
            form.push(("client_secret", secret.to_string()));
        }
        _ => form.push(("client_id", creds.client_id.clone())),
    }
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (status, body) = post_form(token_endpoint, policy, &pairs, basic)
        .await
        .map_err(TokenError::Other)?;
    parse_token_response(status, &body, chrono::Utc::now().timestamp())
}

/// Exchange an authorization code.
#[allow(clippy::too_many_arguments)]
pub async fn exchange_code(
    token_endpoint: &Url,
    policy: OutboundPolicy,
    creds: &ClientCredentials,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    resource: &str,
) -> Result<TokenSet, TokenError> {
    token_request(
        token_endpoint,
        policy,
        creds,
        vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.into()),
            ("redirect_uri", redirect_uri.into()),
            ("code_verifier", code_verifier.into()),
            ("resource", resource.into()),
        ],
    )
    .await
}

/// Use a refresh token. The caller merges the answer with
/// [`merge_refreshed`] (rotation).
pub async fn refresh(
    token_endpoint: &Url,
    policy: OutboundPolicy,
    creds: &ClientCredentials,
    refresh_token: &str,
    resource: &str,
    scope: Option<&str>,
) -> Result<TokenSet, TokenError> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("resource", resource.to_string()),
    ];
    if let Some(s) = scope.filter(|s| !s.is_empty()) {
        form.push(("scope", s.to_string()));
    }
    token_request(token_endpoint, policy, creds, form).await
}

/// Apply a refresh answer to stored OAuth state: a new refresh token replaces
/// the old one (rotation), no new one keeps the old one. Pure.
pub fn merge_refreshed(stored: &mut super::store::OAuthSecrets, fresh: TokenSet) {
    stored.access_token = fresh.access_token;
    if let Some(rt) = fresh.refresh_token {
        stored.refresh_token = Some(rt);
    }
    stored.expires_at = fresh.expires_at;
    if fresh.scope.is_some() {
        stored.scope = fresh.scope;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn www_authenticate_parsing() {
        let c = parse_www_authenticate(
            r#"Bearer realm="mcp", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource/mcp", scope="files:read files:write""#,
        )
        .unwrap();
        assert_eq!(
            c.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(c.scope.as_deref(), Some("files:read files:write"));
        let c = parse_www_authenticate("bearer error=invalid_token").unwrap();
        assert_eq!(c.error.as_deref(), Some("invalid_token"));
        assert!(parse_www_authenticate("Basic realm=\"x\"").is_none());
        assert!(parse_www_authenticate("Bearerish x=1").is_none());
        assert_eq!(parse_www_authenticate("Bearer").unwrap(), BearerChallenge::default());
        // Multi-byte input near the scheme must not panic.
        assert!(parse_www_authenticate("Bearé x=1").is_none());
        assert!(parse_www_authenticate("認證").is_none());
    }

    #[test]
    fn well_known_urls_follow_rfc_9728_and_8414() {
        let prm = protected_resource_metadata_urls(&u("https://mcp.example.com/v1/mcp/?x=1"));
        assert_eq!(prm[0].as_str(), "https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp");
        assert_eq!(prm[1].as_str(), "https://mcp.example.com/.well-known/oauth-protected-resource");
        let root = protected_resource_metadata_urls(&u("https://mcp.example.com/"));
        assert_eq!(root.len(), 1);

        let a = authorization_server_metadata_urls(&u("https://auth.example.com"));
        assert_eq!(a[0].as_str(), "https://auth.example.com/.well-known/oauth-authorization-server");
        assert_eq!(a[1].as_str(), "https://auth.example.com/.well-known/openid-configuration");
        let b = authorization_server_metadata_urls(&u("https://auth.example.com/tenant1"));
        assert_eq!(b[0].as_str(), "https://auth.example.com/.well-known/oauth-authorization-server/tenant1");
        assert_eq!(b[1].as_str(), "https://auth.example.com/.well-known/openid-configuration/tenant1");
        assert_eq!(b[2].as_str(), "https://auth.example.com/tenant1/.well-known/openid-configuration");
    }

    #[test]
    fn canonical_resource_drops_query_fragment_and_trailing_slash() {
        assert_eq!(canonical_resource(&u("https://MCP.Example.com/mcp/?a=1")), "https://mcp.example.com/mcp");
        assert_eq!(canonical_resource(&u("https://mcp.example.com")), "https://mcp.example.com");
    }

    fn meta_doc() -> Value {
        json!({
            "issuer": "https://auth.example.com",
            "authorization_endpoint": "https://auth.example.com/authorize",
            "token_endpoint": "https://auth.example.com/token",
            "registration_endpoint": "https://auth.example.com/register",
            "code_challenge_methods_supported": ["S256"],
        })
    }

    #[test]
    fn as_metadata_requires_matching_issuer_s256_and_safe_endpoints() {
        let p = OutboundPolicy::PUBLIC_ONLY;
        let iss = u("https://auth.example.com/");
        assert!(parse_as_metadata(&meta_doc(), &iss, p).is_ok());

        let mut d = meta_doc();
        d["issuer"] = json!("https://evil.example.com");
        assert!(parse_as_metadata(&d, &iss, p).is_err());

        let mut d = meta_doc();
        d["code_challenge_methods_supported"] = json!(["plain"]);
        assert!(parse_as_metadata(&d, &iss, p).is_err());
        let mut d = meta_doc();
        d.as_object_mut().unwrap().remove("code_challenge_methods_supported");
        assert!(parse_as_metadata(&d, &iss, p).is_err());

        // A token endpoint on a private address or plain http is refused.
        for bad in ["http://auth.example.com/token", "https://10.0.0.1/token", "https://127.0.0.1/token"] {
            let mut d = meta_doc();
            d["token_endpoint"] = json!(bad);
            assert!(parse_as_metadata(&d, &iss, p).is_err(), "{bad}");
        }
    }

    #[test]
    fn pkce_pair_is_s256_and_unique() {
        let (v1, c1) = pkce_pair();
        let (v2, _) = pkce_pair();
        assert_eq!(v1.len(), 43);
        assert_ne!(v1, v2);
        assert_eq!(c1, pkce_challenge(&v1));
        // RFC 7636 appendix B test vector.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_ne!(random_state(), random_state());
        assert_eq!(random_state().len(), 43);
    }

    #[test]
    fn authorize_url_carries_pkce_state_and_resource() {
        let meta = parse_as_metadata(&meta_doc(), &u("https://auth.example.com"), OutboundPolicy::PUBLIC_ONLY).unwrap();
        let url = build_authorize_url(&meta, "cid", "https://dash.example.com/oauth/mcp/callback", "CH", "ST", "https://mcp.example.com/mcp", Some("a b"));
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "CH");
        assert_eq!(q["state"], "ST");
        assert_eq!(q["resource"], "https://mcp.example.com/mcp");
        assert_eq!(q["redirect_uri"], "https://dash.example.com/oauth/mcp/callback");
        assert_eq!(q["scope"], "a b");
    }

    #[test]
    fn registration_body_is_a_public_client_named_duduclaw() {
        let b = registration_body("http://localhost:18789/oauth/mcp/callback", None);
        assert_eq!(b["client_name"], "DuDuClaw");
        assert_eq!(b["token_endpoint_auth_method"], "none");
        assert_eq!(b["redirect_uris"][0], "http://localhost:18789/oauth/mcp/callback");
    }

    #[test]
    fn token_response_parsing_and_errors() {
        let ok = parse_token_response(200, &json!({"access_token": "at", "token_type": "Bearer", "expires_in": 3600, "refresh_token": "rt"}), 1000).unwrap();
        assert_eq!(ok.access_token, "at");
        assert_eq!(ok.expires_at, Some(4600));
        assert_eq!(ok.refresh_token.as_deref(), Some("rt"));
        assert!(matches!(
            parse_token_response(400, &json!({"error": "invalid_grant"}), 0),
            Err(TokenError::InvalidGrant(_))
        ));
        assert!(matches!(parse_token_response(200, &json!({}), 0), Err(TokenError::Other(_))));
        assert!(parse_token_response(200, &json!({"access_token": "a", "token_type": "mac"}), 0).is_err());
    }

    #[test]
    fn refresh_merge_rotates_or_keeps_the_refresh_token() {
        let mut s = crate::remote_mcp::store::OAuthSecrets {
            access_token: "old".into(),
            refresh_token: Some("rt1".into()),
            ..Default::default()
        };
        merge_refreshed(&mut s, TokenSet { access_token: "a2".into(), refresh_token: Some("rt2".into()), expires_at: Some(5), scope: None });
        assert_eq!(s.access_token, "a2");
        assert_eq!(s.refresh_token.as_deref(), Some("rt2"));
        merge_refreshed(&mut s, TokenSet { access_token: "a3".into(), refresh_token: None, expires_at: None, scope: None });
        assert_eq!(s.access_token, "a3");
        assert_eq!(s.refresh_token.as_deref(), Some("rt2"), "no new refresh token keeps the old one");
    }
}
