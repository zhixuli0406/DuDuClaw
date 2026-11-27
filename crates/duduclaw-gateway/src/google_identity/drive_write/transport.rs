//! Exact-table transport for RM-131 Drive/Docs/Sheets calls.
//!
//! `native_confirmation::drive_method_allowed` fences the OLD read transports.
//! Do not call it here: it intentionally rejects our legitimate POST/PATCH
//! calls (and multipart uploads). This module's closed table is the authority.
//! No URL, query, MIME header, or trash body comes from model-supplied strings.

use std::{fmt, future::Future, pin::Pin, time::Duration};

use axum::body::Bytes;
use reqwest::{header, Method, Url};
use serde_json::Value;
use zeroize::Zeroizing;

const JSON_LIMIT: usize = 1024 * 1024;
const CONTENT_LIMIT: usize = 5 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const MAX_REQUESTS: usize = 26;
const TIMEOUT: Duration = Duration::from_secs(30);
const SCOPE_FIELDS: &str = "id,name,mimeType,parents,driveId,trashed,shortcutDetails,capabilities(canEdit,canRename,canAddChildren,canTrash,canMoveItemWithinDrive)";
// Includes parents for the create/copy/move post-checks in the scope layer.
const WRITE_FIELDS: &str = "id,name,mimeType,parents,driveId,trashed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotSentReason {
    InvalidId,
    InvalidJson,
    JsonTooLarge,
    ContentTooLarge,
    InvalidBearer,
    InvalidEndpoint,
    InvalidParents,
    RequestLimit,
    BoundaryUnavailable,
    ClientUnavailable,
}

/// Only this constructor can introduce an ID. No unchecked Deserialize impl.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GoogleId(String);

impl GoogleId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, NotSentReason> {
        let value = value.into();
        if !(10..=128).contains(&value.len())
            || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            // "permissions" happens to meet the length/alphabet rule.
            || matches!(value.as_str(), "permissions" | "revisions" | "comments" | "emptyTrash")
        {
            return Err(NotSentReason::InvalidId);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str { &self.0 }
}

impl fmt::Debug for GoogleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleId([redacted])")
    }
}

/// Validated, serialized JSON object. Batch calls take the whole request
/// envelope, e.g. {"requests":[...]}; operation schemas belong to P5b/P5c.
#[derive(Clone)]
pub(crate) struct JsonBody(Vec<u8>);

impl JsonBody {
    pub(crate) fn new(value: Value) -> Result<Self, NotSentReason> {
        if !value.is_object() { return Err(NotSentReason::InvalidJson); }
        let bytes = serde_json::to_vec(&value).map_err(|_| NotSentReason::InvalidJson)?;
        if bytes.len() > JSON_LIMIT { return Err(NotSentReason::JsonTooLarge); }
        Ok(Self(bytes))
    }
}

impl fmt::Debug for JsonBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JsonBody([redacted])")
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum MetadataFields { Scope }

impl MetadataFields {
    fn as_str(self) -> &'static str {
        match self { Self::Scope => SCOPE_FIELDS }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ClosedMime {
    PlainText,
    Csv,
    Markdown,
    /// Only for attachments whose type is unknown. The tool program must
    /// establish attachment provenance; this is not a model-selectable MIME.
    UnknownAttachment,
}

impl ClosedMime {
    fn as_str(self) -> &'static str {
        match self {
            Self::PlainText => "text/plain",
            Self::Csv => "text/csv",
            Self::Markdown => "text/markdown",
            Self::UnknownAttachment => "application/octet-stream",
        }
    }
}

#[derive(Clone)]
pub(crate) enum WriteCall {
    DriveProbe,
    DriveGetMetadata { file_id: GoogleId, fields: MetadataFields },
    DriveCreateFolder { metadata_json: JsonBody },
    DriveCreateMultipart { metadata_json: JsonBody, content: Bytes, content_type: ClosedMime },
    DriveCopy { file_id: GoogleId, metadata_json: JsonBody },
    DriveUpdateMetadata { file_id: GoogleId, metadata_json: JsonBody },
    DriveUpdateContent { file_id: GoogleId, metadata_json: JsonBody, content: Bytes, content_type: ClosedMime },
    DriveMove { file_id: GoogleId, add_parent: GoogleId, remove_parents: Vec<GoogleId> },
    DriveTrash { file_id: GoogleId },
    DocsGet { document_id: GoogleId, include_tabs: bool },
    DocsBatchUpdate { document_id: GoogleId, requests_json: JsonBody },
    SheetsGetProperties { spreadsheet_id: GoogleId },
    SheetsBatchUpdate { spreadsheet_id: GoogleId, requests_json: JsonBody },
}

impl fmt::Debug for WriteCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteCall([redacted])")
    }
}

// A single closed table supplies both construction and final validation.
// Dynamic path segments can only originate in GoogleId (or its batch suffix).
struct Route {
    method: Method,
    host: &'static str,
    segments: Vec<String>,
    query: Vec<(&'static str, String)>,
}

impl WriteCall {
    fn validate_parents(&self) -> Result<(), NotSentReason> {
        if let Self::DriveMove { remove_parents, .. } = self {
            if remove_parents.is_empty() || remove_parents.len() > 10
                || remove_parents.iter().enumerate().any(|(index, parent)| remove_parents[..index].contains(parent))
            {
                return Err(NotSentReason::InvalidParents);
            }
        }
        Ok(())
    }

    fn route(&self) -> Route {
        let (method, host, parts): (Method, &str, Vec<String>) = match self {
            Self::DriveProbe =>
                (Method::GET, "www.googleapis.com", vec!["drive".into(), "v3".into(), "files".into()]),
            Self::DriveGetMetadata { file_id, .. } =>
                (Method::GET, "www.googleapis.com", vec!["drive".into(), "v3".into(), "files".into(), file_id.0.clone()]),
            Self::DriveCreateFolder { .. } =>
                (Method::POST, "www.googleapis.com", vec!["drive".into(), "v3".into(), "files".into()]),
            Self::DriveCreateMultipart { .. } =>
                (Method::POST, "www.googleapis.com", vec!["upload".into(), "drive".into(), "v3".into(), "files".into()]),
            Self::DriveCopy { file_id, .. } =>
                (Method::POST, "www.googleapis.com", vec!["drive".into(), "v3".into(), "files".into(), file_id.0.clone(), "copy".into()]),
            Self::DriveUpdateMetadata { file_id, .. } | Self::DriveMove { file_id, .. } | Self::DriveTrash { file_id } =>
                (Method::PATCH, "www.googleapis.com", vec!["drive".into(), "v3".into(), "files".into(), file_id.0.clone()]),
            Self::DriveUpdateContent { file_id, .. } =>
                (Method::PATCH, "www.googleapis.com", vec!["upload".into(), "drive".into(), "v3".into(), "files".into(), file_id.0.clone()]),
            Self::DocsGet { document_id, .. } =>
                (Method::GET, "docs.googleapis.com", vec!["v1".into(), "documents".into(), document_id.0.clone()]),
            Self::DocsBatchUpdate { document_id, .. } =>
                (Method::POST, "docs.googleapis.com", vec!["v1".into(), "documents".into(), format!("{}:batchUpdate", document_id.as_str())]),
            Self::SheetsGetProperties { spreadsheet_id } =>
                (Method::GET, "sheets.googleapis.com", vec!["v4".into(), "spreadsheets".into(), spreadsheet_id.0.clone()]),
            Self::SheetsBatchUpdate { spreadsheet_id, .. } =>
                (Method::POST, "sheets.googleapis.com", vec!["v4".into(), "spreadsheets".into(), format!("{}:batchUpdate", spreadsheet_id.as_str())]),
        };
        let query = match self {
            Self::DriveProbe => vec![
                ("pageSize", "1".into()), ("fields", "files(id)".into()),
                ("supportsAllDrives", "true".into()), ("includeItemsFromAllDrives", "true".into()),
            ],
            Self::DriveGetMetadata { fields, .. } => vec![
                ("fields", fields.as_str().into()), ("supportsAllDrives", "true".into()),
            ],
            Self::DriveCreateFolder { .. } | Self::DriveCopy { .. }
            | Self::DriveUpdateMetadata { .. } | Self::DriveTrash { .. } => vec![
                ("supportsAllDrives", "true".into()), ("fields", WRITE_FIELDS.into()),
            ],
            Self::DriveCreateMultipart { .. } | Self::DriveUpdateContent { .. } => vec![
                ("uploadType", "multipart".into()), ("supportsAllDrives", "true".into()), ("fields", WRITE_FIELDS.into()),
            ],
            Self::DriveMove { add_parent, remove_parents, .. } => vec![
                ("addParents", add_parent.0.clone()),
                ("removeParents", remove_parents.iter().map(GoogleId::as_str).collect::<Vec<_>>().join(",")),
                ("supportsAllDrives", "true".into()), ("fields", WRITE_FIELDS.into()),
            ],
            Self::DocsGet { include_tabs: true, .. } => vec![("includeTabsContent", "true".into())],
            Self::SheetsGetProperties { .. } => vec![("fields", "sheets.properties".into())],
            Self::DocsGet { include_tabs: false, .. } | Self::DocsBatchUpdate { .. }
            | Self::SheetsBatchUpdate { .. } => vec![],
        };
        Route { method, host, segments: parts, query }
    }

    pub(crate) fn allowed_query_keys(&self) -> &'static [&'static str] {
        match self {
            Self::DriveProbe => &["pageSize", "fields", "supportsAllDrives", "includeItemsFromAllDrives"],
            Self::DriveGetMetadata { .. } => &["fields", "supportsAllDrives"],
            Self::DriveCreateFolder { .. } | Self::DriveCopy { .. }
            | Self::DriveUpdateMetadata { .. } | Self::DriveTrash { .. } => &["supportsAllDrives", "fields"],
            Self::DriveCreateMultipart { .. } | Self::DriveUpdateContent { .. } => &["uploadType", "supportsAllDrives", "fields"],
            Self::DriveMove { .. } => &["addParents", "removeParents", "supportsAllDrives", "fields"],
            Self::DocsGet { include_tabs: true, .. } => &["includeTabsContent"],
            Self::SheetsGetProperties { .. } => &["fields"],
            Self::DocsGet { include_tabs: false, .. } | Self::DocsBatchUpdate { .. }
            | Self::SheetsBatchUpdate { .. } => &[],
        }
    }

    pub(crate) fn method_and_url(&self) -> (Method, Url) {
        let route = self.route();
        let mut url = Url::parse(&format!("https://{}/", route.host)).expect("static Google origin");
        url.set_path(&format!("/{}", route.segments.join("/")));
        if !route.query.is_empty() {
            url.query_pairs_mut().extend_pairs(route.query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        if matches!(self, Self::DriveProbe) {
            url.set_query(Some("pageSize=1&fields=files(id)&supportsAllDrives=true&includeItemsFromAllDrives=true"));
        }
        (route.method, url)
    }

    fn validate_target(&self, method: &Method, url: &Url) -> Result<(), NotSentReason> {
        self.validate_parents()?;
        if url.as_str().len() > 8192 { return Err(NotSentReason::InvalidEndpoint); }
        let route = self.route();
        let segments = url.path_segments().ok_or(NotSentReason::InvalidEndpoint)?.collect::<Vec<_>>();
        let query = url.query_pairs().collect::<Vec<_>>();
        if *method != route.method || url.scheme() != "https"
            || url.host_str() != Some(route.host) || url.port().is_some()
            || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some()
            || segments != route.segments.iter().map(String::as_str).collect::<Vec<_>>()
            || query.len() != route.query.len()
            || query.iter().zip(&route.query).any(|((key, value), (expected_key, expected_value))| {
                !self.allowed_query_keys().contains(&key.as_ref())
                    || key.as_ref() != *expected_key || value.as_ref() != expected_value.as_str()
            })
            || (route.query.is_empty() && url.query().is_some())
        {
            return Err(NotSentReason::InvalidEndpoint);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_body(&self) -> Result<Option<(String, Vec<u8>)>, NotSentReason> {
        self.body()
    }

    fn body(&self) -> Result<Option<(String, Vec<u8>)>, NotSentReason> {
        match self {
            Self::DriveProbe | Self::DriveGetMetadata { .. } | Self::DocsGet { .. } | Self::SheetsGetProperties { .. } => Ok(None),
            Self::DriveTrash { .. } => Ok(Some(("application/json".into(), br#"{"trashed":true}"#.to_vec()))),
            Self::DriveMove { .. } => Ok(Some(("application/json".into(), b"{}".to_vec()))),
            Self::DriveCreateFolder { metadata_json } | Self::DriveCopy { metadata_json, .. }
            | Self::DriveUpdateMetadata { metadata_json, .. }
            | Self::DocsBatchUpdate { requests_json: metadata_json, .. }
            | Self::SheetsBatchUpdate { requests_json: metadata_json, .. } => {
                check_json(metadata_json)?;
                Ok(Some(("application/json".into(), metadata_json.0.clone())))
            }
            Self::DriveCreateMultipart { metadata_json, content, content_type }
            | Self::DriveUpdateContent { metadata_json, content, content_type, .. } => {
                multipart(metadata_json, content, *content_type).map(Some)
            }
        }
    }
}

fn check_json(body: &JsonBody) -> Result<(), NotSentReason> {
    if body.0.len() > JSON_LIMIT { return Err(NotSentReason::JsonTooLarge); }
    Ok(())
}

fn multipart(metadata: &JsonBody, content: &Bytes, mime: ClosedMime) -> Result<(String, Vec<u8>), NotSentReason> {
    check_json(metadata)?;
    if content.len() > CONTENT_LIMIT { return Err(NotSentReason::ContentTooLarge); }
    // Bounded collision loop; scan both parts, not merely delimiter-shaped lines.
    for _ in 0..8 {
        let boundary = format!("duduclaw_{}", uuid::Uuid::new_v4().simple());
        if boundary_occurs(&boundary, content) || boundary_occurs(&boundary, &metadata.0) { continue; }
        let mut body = format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").into_bytes();
        body.extend_from_slice(&metadata.0);
        body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: {}\r\n\r\n", mime.as_str()).as_bytes());
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        return Ok((format!("multipart/related; boundary={boundary}"), body));
    }
    Err(NotSentReason::BoundaryUnavailable)
}

fn boundary_occurs(boundary: &str, bytes: &[u8]) -> bool {
    bytes.windows(boundary.len()).any(|window| window == boundary.as_bytes())
}

/// The authenticated request is opaque outside this transport. Do not derive
/// Debug: even a response or a user-controlled body may reflect the bearer.
pub(crate) struct BuiltRequest(reqwest::Request);

#[cfg(test)]
impl BuiltRequest {
    pub(super) fn into_test_request(self) -> reqwest::Request { self.0 }
}

impl fmt::Debug for BuiltRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BuiltRequest([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendError { Network, Timeout, ResponseTooLarge }

/// Implementations must send once, never redirect/retry, and cap streamed
/// responses. All errors after entering send are conservatively MaybeSent.
pub(crate) trait WriteHttp: Send + Sync {
    fn send(&self, request: BuiltRequest)
        -> Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), SendError>> + Send + '_>>;
}

struct ReqwestHttp { client: reqwest::Client }

impl WriteHttp for ReqwestHttp {
    fn send(&self, request: BuiltRequest)
        -> Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), SendError>> + Send + '_>>
    {
        Box::pin(async move {
            let mut response = self.client.execute(request.0).await.map_err(|_| SendError::Network)?;
            let status = response.status().as_u16();
            if response.content_length().is_some_and(|n| n > RESPONSE_LIMIT as u64) {
                return Err(SendError::ResponseTooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| SendError::Network)? {
                if chunk.len() > RESPONSE_LIMIT - body.len() {
                    return Err(SendError::ResponseTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok((status, body))
        })
    }
}

/// NotSent describes this request only; earlier steps in the same tool call
/// may have succeeded. MaybeSent must become result_unconfirmed, never resend.
pub(crate) enum WriteResult {
    Ok((u16, Vec<u8>)),
    NotSent(NotSentReason),
    MaybeSent(SendError),
}

impl fmt::Debug for WriteResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok((status, _)) => f.debug_tuple("Ok").field(status).field(&"[redacted]").finish(),
            Self::NotSent(reason) => f.debug_tuple("NotSent").field(reason).finish(),
            Self::MaybeSent(reason) => f.debug_tuple("MaybeSent").field(reason).finish(),
        }
    }
}

/// Create once per tool invocation; the 26-request budget includes metadata
/// reads and failed sends. No public reset or production HTTP injection.
pub(crate) struct WriteTransport {
    bearer: Zeroizing<String>,
    requests: usize,
    http: Box<dyn WriteHttp>,
    #[cfg(test)]
    test_api_base: Option<Url>,
    #[cfg(test)]
    test_target: Option<(Method, Url)>,
}

impl fmt::Debug for WriteTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteTransport").field("requests", &self.requests).finish_non_exhaustive()
    }
}

impl WriteTransport {
    pub(crate) fn new(bearer: Zeroizing<String>) -> Result<Self, NotSentReason> {
        let client = crate::isolation_connector_clients::client_builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build().map_err(|_| NotSentReason::ClientUnavailable)?;
        Ok(Self {
            bearer, requests: 0, http: Box::new(ReqwestHttp { client }),
            #[cfg(test)]
            test_api_base: None,
            #[cfg(test)]
            test_target: None,
        })
    }

    #[cfg(test)]
    pub(super) fn with_http(bearer: Zeroizing<String>, http: Box<dyn WriteHttp>) -> Self {
        Self { bearer, requests: 0, http, test_api_base: None, test_target: None }
    }

    /// Instance-local override: parallel tests cannot steal each other's token.
    #[cfg(test)]
    pub(crate) fn set_test_api_base(&mut self, base: Url) -> Result<(), NotSentReason> {
        let loopback = match base.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !loopback || base.scheme() != "http" || base.path() != "/"
            || !base.username().is_empty() || base.password().is_some()
            || base.query().is_some() || base.fragment().is_some()
        {
            return Err(NotSentReason::InvalidEndpoint);
        }
        self.test_api_base = Some(base);
        Ok(())
    }

    pub(crate) async fn send(&mut self, call: &WriteCall) -> WriteResult {
        let request = match self.prepare(call) {
            Ok(request) => request,
            Err(reason) => return WriteResult::NotSent(reason),
        };
        self.requests += 1;
        // Includes response streaming; dropping the future does not prove that
        // Google did not apply the write. There is exactly one send invocation.
        match tokio::time::timeout(TIMEOUT, self.http.send(request)).await {
            Ok(Ok((_, ref body))) if body.len() > RESPONSE_LIMIT => WriteResult::MaybeSent(SendError::ResponseTooLarge),
            Ok(Ok(response)) => WriteResult::Ok(response),
            Ok(Err(reason)) => WriteResult::MaybeSent(reason),
            Err(_) => WriteResult::MaybeSent(SendError::Timeout),
        }
    }

    fn prepare(&mut self, call: &WriteCall) -> Result<BuiltRequest, NotSentReason> {
        if self.requests >= MAX_REQUESTS { return Err(NotSentReason::RequestLimit); }
        // RFC 6750 alphabet; reject CR/LF and empty credentials locally.
        if self.bearer.is_empty() || self.bearer.len() > 16384
            || !self.bearer.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'='))
        {
            return Err(NotSentReason::InvalidBearer);
        }
        // Bound the list before route construction joins its values.
        call.validate_parents()?;
        let (method, url) = call.method_and_url();
        let mut request = reqwest::Request::new(method, url);
        if let Some((content_type, body)) = call.body()? {
            request.headers_mut().insert(header::CONTENT_TYPE,
                header::HeaderValue::from_str(&content_type).map_err(|_| NotSentReason::InvalidEndpoint)?);
            *request.body_mut() = Some(body.into());
        }
        #[cfg(test)]
        if let Some((method, url)) = self.test_target.take() {
            *request.method_mut() = method;
            *request.url_mut() = url;
        }
        // Validate the final logical request, including every query VALUE and
        // duplicate key, before adding credentials or applying the test base.
        call.validate_target(request.method(), request.url())?;
        #[cfg(test)]
        if let Some(base) = &self.test_api_base {
            let mut actual = base.clone();
            actual.set_path(request.url().path());
            actual.set_query(request.url().query());
            *request.url_mut() = actual;
        }
        let auth = Zeroizing::new(format!("Bearer {}", self.bearer.as_str()));
        let mut auth = header::HeaderValue::from_str(auth.as_str()).map_err(|_| NotSentReason::InvalidBearer)?;
        auth.set_sensitive(true);
        request.headers_mut().insert(header::AUTHORIZATION, auth);
        Ok(BuiltRequest(request))
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
