//! MCP protocol revisions this server speaks, and the per-era envelope rules.
//!
//! Since MCP 2026-07-28 there are two eras (spec "Versioning and
//! Compatibility"):
//!
//! - **legacy** (2025-11-25 and earlier): the client opens with `initialize`,
//!   the server answers with one negotiated revision for the whole stdio
//!   process or HTTP exchange;
//! - **modern** (2026-07-28 and later): no handshake. Every request carries
//!   its revision in `params._meta["io.modelcontextprotocol/protocolVersion"]`
//!   and the server accepts or rejects each request on its own.
//!
//! This server is dual-era: an `initialize` request selects legacy semantics,
//! a request carrying the modern `_meta` key is served statelessly under the
//! modern rules, and both transports (stdio `mcp-server`, Streamable HTTP
//! `/mcp`) share the helpers below so they cannot drift apart.

use serde_json::{Value, json};

/// Modern revisions (per-request `_meta`). Newest first.
pub(crate) const MODERN_PROTOCOL_VERSIONS: &[&str] = &["2026-07-28"];

/// Legacy revisions (`initialize` handshake). Newest first.
pub(crate) const LEGACY_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The legacy revision offered when `initialize` asks for one we do not know
/// (legacy rule: the server answers with its latest supported version).
pub(crate) const LATEST_LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// `_meta` key carrying a modern request's protocol revision.
pub(crate) const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key the server uses to identify itself on modern results.
pub(crate) const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC code of `UnsupportedProtocolVersionError` (2026-07-28).
pub(crate) const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Every revision this server supports, modern first, as listed in
/// `UnsupportedProtocolVersionError.data.supported`.
pub(crate) fn supported_versions() -> Vec<&'static str> {
    MODERN_PROTOCOL_VERSIONS
        .iter()
        .chain(LEGACY_PROTOCOL_VERSIONS)
        .copied()
        .collect()
}

/// Whether `v` is a revision this server supports in either era.
pub(crate) fn is_supported(v: &str) -> bool {
    MODERN_PROTOCOL_VERSIONS.contains(&v) || LEGACY_PROTOCOL_VERSIONS.contains(&v)
}

/// The legacy revision to answer an `initialize` with: the client's when we
/// support it, otherwise our latest legacy one. A modern revision requested
/// through `initialize` also gets the latest legacy one, because `initialize`
/// only exists in the legacy era.
pub(crate) fn negotiate_legacy(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| LEGACY_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(LATEST_LEGACY_PROTOCOL_VERSION)
}

/// `name` / `title` / `version` the server reports about itself.
pub(crate) fn server_info() -> Value {
    json!({
        "name": "duduclaw",
        "title": "DuDuClaw",
        "version": duduclaw_gateway::updater::current_version()
    })
}

/// The `initialize` result for a legacy client. `capabilities` differs per
/// transport (stdio can push `tools/list_changed`, stateless HTTP cannot).
pub(crate) fn initialize_result(params: &Value, capabilities: Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    json!({
        "protocolVersion": negotiate_legacy(requested),
        "capabilities": capabilities,
        "serverInfo": server_info(),
    })
}

/// How one request asks to be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestEra {
    /// No modern `_meta` revision: legacy semantics.
    Legacy,
    /// A supported modern revision in `_meta`.
    Modern(&'static str),
    /// A `_meta` revision this server does not implement (or not a string).
    Unsupported(String),
}

/// Classify a request by its `params._meta` protocol revision.
///
/// A legacy revision named in `_meta` is unsupported: legacy revisions are
/// only reachable through `initialize`, never per request.
pub(crate) fn request_era(params: &Value) -> RequestEra {
    let Some(raw) = params
        .get("_meta")
        .and_then(|m| m.get(META_PROTOCOL_VERSION))
    else {
        return RequestEra::Legacy;
    };
    match raw.as_str() {
        Some(v) => match MODERN_PROTOCOL_VERSIONS.iter().find(|m| **m == v) {
            Some(m) => RequestEra::Modern(m),
            None => RequestEra::Unsupported(v.to_string()),
        },
        // Echoed back in the error: keep a non-string value short.
        None => {
            RequestEra::Unsupported(duduclaw_core::truncate_chars(&raw.to_string(), 64).to_string())
        }
    }
}

/// `UnsupportedProtocolVersionError` for request `id`.
pub(crate) fn unsupported_version_error(id: &Value, requested: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": UNSUPPORTED_PROTOCOL_VERSION,
            "message": "Unsupported protocol version",
            "data": { "supported": supported_versions(), "requested": requested }
        }
    })
}

/// `_meta` key carrying a modern request's client capabilities (required).
pub(crate) const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// `_meta` key tagging a notification with its `subscriptions/listen` id.
pub(crate) const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

/// JSON-RPC code of `HeaderMismatchError` (2026-07-28, HTTP only).
pub(crate) const HEADER_MISMATCH: i64 = -32020;

/// The sub-range 2026-07-28 reserves for codes the specification defines.
const MCP_RESERVED_CODES: std::ops::RangeInclusive<i64> = -32099..=-32020;
/// Reserved-range codes the specification actually defines.
const MCP_DEFINED_CODES: &[i64] = &[HEADER_MISMATCH, -32021, UNSUPPORTED_PROTOCOL_VERSION];
/// Codes the 2026-07-28 drafts gave to header mismatch, missing client
/// capability and unsupported version before renumbering them. DuDuClaw uses
/// `-32003` for authentication failures; a modern client must not mistake
/// that for a draft capability error.
const DRAFT_CODES: &[i64] = &[-32001, -32003, -32004];
/// Where those codes go on a modern response: the implementation-defined
/// server error. The original code stays in `error.data.duduclawCode`.
const IMPLEMENTATION_ERROR: i64 = -32000;

/// Most `subscriptions/listen` streams one stdio process keeps open.
pub(crate) const MAX_SUBSCRIPTIONS: usize = 16;

/// Cache hint for a modern `tools/list`: the stdio watcher re-checks the
/// visible set this often, and a grant can change it at any time.
pub(crate) const TOOLS_LIST_TTL_MS: u64 = 5_000;
/// Cache hint for `server/discover`: versions and capabilities only change
/// with a new binary.
pub(crate) const DISCOVER_TTL_MS: u64 = 300_000;

/// A modern request must carry `clientCapabilities` next to its revision
/// (spec: a request missing a required `_meta` field is malformed, `-32602`).
pub(crate) fn modern_meta_problem(params: &Value) -> Option<String> {
    match params
        .get("_meta")
        .and_then(|m| m.get(META_CLIENT_CAPABILITIES))
    {
        Some(Value::Object(_)) => None,
        Some(_) => Some(format!(
            "_meta[\"{META_CLIENT_CAPABILITIES}\"] must be an object"
        )),
        None => Some(format!(
            "missing required _meta[\"{META_CLIENT_CAPABILITIES}\"]"
        )),
    }
}

/// Turn a JSON-RPC response into its modern form. A `result` object gains
/// `resultType: "complete"` and the server's identity in `_meta`; an error
/// that uses an undefined code from the MCP-reserved range is moved to
/// `-32000` (the spec forbids emitting undefined reserved codes).
pub(crate) fn modernize_response(mut response: Value) -> Value {
    if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
        result
            .entry("resultType")
            .or_insert_with(|| json!("complete"));
        let meta = result.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(META_SERVER_INFO.to_string(), server_info());
        }
    }
    if let Some(error) = response.get_mut("error").and_then(Value::as_object_mut) {
        let code = error.get("code").and_then(Value::as_i64);
        if let Some(code) = code.filter(|c| {
            (MCP_RESERVED_CODES.contains(c) && !MCP_DEFINED_CODES.contains(c))
                || DRAFT_CODES.contains(c)
        }) {
            error.insert("code".into(), json!(IMPLEMENTATION_ERROR));
            let data = error.entry("data").or_insert_with(|| json!({}));
            if let Some(data) = data.as_object_mut() {
                data.insert("duduclawCode".into(), json!(code));
            }
        }
    }
    response
}

/// Add the `CacheableResult` hints to a `result` (modern `tools/list` and
/// `server/discover`). Listings depend on the caller's key and capabilities,
/// so the scope is always `private`.
pub(crate) fn with_cache_hints(mut response: Value, ttl_ms: u64) -> Value {
    if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("ttlMs".into(), json!(ttl_ms));
        result.insert("cacheScope".into(), json!("private"));
    }
    response
}

/// The `server/discover` result (before [`modernize_response`]).
pub(crate) fn discover_result(capabilities: Value) -> Value {
    json!({
        "supportedVersions": supported_versions(),
        "capabilities": capabilities,
        "ttlMs": DISCOVER_TTL_MS,
        "cacheScope": "private",
    })
}

/// Whether a `subscriptions/listen` request asks for tool-list changes.
pub(crate) fn wants_tools_list_changed(params: &Value) -> bool {
    params
        .get("notifications")
        .and_then(|n| n.get("toolsListChanged"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `notifications/subscriptions/acknowledged` for subscription `id`, listing
/// only the notification types this server will actually deliver.
pub(crate) fn subscription_ack(id: &Value, tools_list_changed: bool) -> Value {
    let honored = if tools_list_changed {
        json!({ "toolsListChanged": true })
    } else {
        json!({})
    };
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": { "_meta": { META_SUBSCRIPTION_ID: id }, "notifications": honored }
    })
}

/// A modern `notifications/tools/list_changed`, tagged with its subscription.
pub(crate) fn tagged_tools_list_changed(subscription_id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": { "_meta": { META_SUBSCRIPTION_ID: subscription_id } }
    })
}

/// The completion result that ends subscription `id` from the server side.
pub(crate) fn subscription_complete(id: &Value) -> Value {
    modernize_response(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "_meta": { META_SUBSCRIPTION_ID: id } }
    }))
}

/// Decode an HTTP `Mcp-Name` value: plain text, or the RFC 2047-style
/// `=?base64?…?=` form used for values that are not header-safe.
pub(crate) fn decode_header_value(raw: &str) -> Option<String> {
    use base64::Engine as _;
    match raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok()),
        None => Some(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_negotiation_echoes_known_and_falls_back_to_latest() {
        assert_eq!(negotiate_legacy(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_legacy(Some("2024-11-05")), "2024-11-05");
        assert_eq!(
            negotiate_legacy(Some("1999-01-01")),
            LATEST_LEGACY_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_legacy(None), LATEST_LEGACY_PROTOCOL_VERSION);
        // `initialize` never yields a modern revision.
        assert_eq!(
            negotiate_legacy(Some("2026-07-28")),
            LATEST_LEGACY_PROTOCOL_VERSION
        );
    }

    #[test]
    fn request_era_reads_the_meta_revision() {
        assert_eq!(request_era(&json!({})), RequestEra::Legacy);
        assert_eq!(request_era(&Value::Null), RequestEra::Legacy);
        assert_eq!(
            request_era(&json!({"_meta": {META_PROTOCOL_VERSION: "2026-07-28"}})),
            RequestEra::Modern("2026-07-28")
        );
        assert_eq!(
            request_era(&json!({"_meta": {META_PROTOCOL_VERSION: "2025-11-25"}})),
            RequestEra::Unsupported("2025-11-25".into())
        );
        assert!(matches!(
            request_era(&json!({"_meta": {META_PROTOCOL_VERSION: 7}})),
            RequestEra::Unsupported(_)
        ));
    }

    #[test]
    fn unsupported_error_lists_every_supported_revision() {
        let e = unsupported_version_error(&json!(1), "1900-01-01");
        assert_eq!(e["error"]["code"], -32022);
        assert_eq!(e["error"]["data"]["requested"], "1900-01-01");
        let supported = e["error"]["data"]["supported"].as_array().unwrap();
        assert_eq!(supported[0], "2026-07-28");
        assert!(supported.contains(&json!("2024-11-05")));
    }

    #[test]
    fn modernize_adds_result_type_and_server_info_only_to_results() {
        let ok = modernize_response(json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}));
        assert_eq!(ok["result"]["resultType"], "complete");
        assert_eq!(ok["result"]["_meta"][META_SERVER_INFO]["name"], "duduclaw");
        let err = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "x"}});
        assert_eq!(modernize_response(err.clone()), err);
    }

    #[test]
    fn modernize_moves_undefined_reserved_codes_out_of_the_mcp_range() {
        let limited = modernize_response(
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32029, "message": "slow down"}}),
        );
        assert_eq!(limited["error"]["code"], -32000);
        assert_eq!(limited["error"]["data"]["duduclawCode"], -32029);
        // Spec-defined reserved codes and ordinary codes are left alone.
        let auth = modernize_response(
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32003, "message": "auth"}}),
        );
        assert_eq!(auth["error"]["code"], -32000);
        assert_eq!(auth["error"]["data"]["duduclawCode"], -32003);
        for code in [-32022, -32020, -32602, -32601] {
            let e = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": "m"}});
            assert_eq!(modernize_response(e.clone()), e, "{code}");
        }
    }

    #[test]
    fn modern_meta_requires_client_capabilities_object() {
        let ok =
            json!({"_meta": {META_PROTOCOL_VERSION: "2026-07-28", META_CLIENT_CAPABILITIES: {}}});
        assert_eq!(modern_meta_problem(&ok), None);
        let missing = json!({"_meta": {META_PROTOCOL_VERSION: "2026-07-28"}});
        assert!(modern_meta_problem(&missing).is_some());
        let wrong = json!({"_meta": {META_CLIENT_CAPABILITIES: "x"}});
        assert!(modern_meta_problem(&wrong).is_some());
    }

    #[test]
    fn subscription_messages_carry_the_listen_id() {
        let ack = subscription_ack(&json!("listen-1"), true);
        assert_eq!(ack["params"]["_meta"][META_SUBSCRIPTION_ID], "listen-1");
        assert_eq!(ack["params"]["notifications"]["toolsListChanged"], true);
        assert_eq!(
            subscription_ack(&json!(1), false)["params"]["notifications"],
            json!({})
        );
        let n = tagged_tools_list_changed(&json!(7));
        assert_eq!(n["method"], "notifications/tools/list_changed");
        assert_eq!(n["params"]["_meta"][META_SUBSCRIPTION_ID], 7);
        assert!(n.get("id").is_none());
        let done = subscription_complete(&json!(7));
        assert_eq!(done["id"], 7);
        assert_eq!(done["result"]["resultType"], "complete");
        assert_eq!(done["result"]["_meta"][META_SUBSCRIPTION_ID], 7);
        assert!(wants_tools_list_changed(
            &json!({"notifications": {"toolsListChanged": true}})
        ));
        assert!(!wants_tools_list_changed(&json!({"notifications": {}})));
    }

    #[test]
    fn cache_hints_and_discover_shape() {
        let r = with_cache_hints(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}),
            5,
        );
        assert_eq!(r["result"]["ttlMs"], 5);
        assert_eq!(r["result"]["cacheScope"], "private");
        let d = discover_result(json!({"tools": {}}));
        assert_eq!(d["supportedVersions"][0], "2026-07-28");
        assert!(d["ttlMs"].as_u64().is_some());
    }

    #[test]
    fn header_values_decode_plain_and_base64() {
        assert_eq!(
            decode_header_value("memory_search").as_deref(),
            Some("memory_search")
        );
        assert_eq!(
            decode_header_value("=?base64?5L2g5aW9?=").as_deref(),
            Some("你好")
        );
        assert_eq!(decode_header_value("=?base64?***?="), None);
    }
}
