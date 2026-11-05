//! Shared wire rules for the dual-generation MCP clients.

use std::collections::HashSet;

use base64::Engine as _;
use serde_json::{Value, json};

pub const MODERN: &str = "2026-07-28";
pub const SUPPORTED: &[&str] = &[MODERN, "2025-11-25", "2025-06-18"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const CLIENT_KEY: &str = "io.modelcontextprotocol/clientInfo";
const MAX_SCHEMA_DEPTH: usize = 64;
const MAX_SCHEMA_NODES: usize = 10_000;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Hard limit for one stdio frame, excluding the newline delimiter.
pub const MAX_STDIO_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Read incrementally, refusing oversized frames before copying their bytes.
/// After an error the caller must close the stream: its frame boundary is no
/// longer known. EOF may terminate the last frame without LF.
pub async fn read_stdio_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    use tokio::io::AsyncBufReadExt;
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if frame.is_empty() { return Ok(None); }
            break;
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.unwrap_or(available.len());
        if count > MAX_STDIO_FRAME_BYTES - frame.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData, "MCP stdio frame too large",
            ));
        }
        // Bound capacity as well as length, including for unusual chunk sizes.
        let needed = frame.len() + count;
        if needed > frame.capacity() {
            let capacity = needed.max(frame.capacity() * 2).min(MAX_STDIO_FRAME_BYTES);
            frame.reserve_exact(capacity - frame.len());
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() { break; }
    }
    String::from_utf8(frame).map(Some).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid MCP stdio UTF-8")
    })
}

/// Preference is local, never the order of an untrusted server's list.
pub fn best_supported(versions: &Value) -> Option<&'static str> {
    let versions = versions.as_array()?;
    SUPPORTED.iter().copied().find(|candidate| {
        versions.iter().any(|version| version.as_str() == Some(*candidate))
    })
}

/// Add host-owned metadata without losing extension metadata or arguments.
/// Malformed non-object parameters are left intact for the caller to reject.
pub fn inject_meta(frame: &mut Value, version: &str, capabilities: &Value) {
    if version != MODERN { return; }
    let Some(frame) = frame.as_object_mut() else { return };
    let params = frame.entry("params").or_insert_with(|| json!({}));
    if params.is_null() { *params = json!({}); }
    let Some(params) = params.as_object_mut() else { return };
    let meta = params.entry("_meta").or_insert_with(|| json!({}));
    if meta.is_null() { *meta = json!({}); }
    let Some(meta) = meta.as_object_mut() else { return };
    meta.insert(VERSION_KEY.into(), json!(version));
    meta.insert(CAPABILITIES_KEY.into(), capabilities.clone());
    meta.insert(CLIENT_KEY.into(), json!({
        "name": "DuDuClaw", "version": env!("CARGO_PKG_VERSION"),
    }));
}

/// Header-safe ASCII is preserved; the sentinel itself must also be escaped.
pub fn encode_header_value(value: &str) -> String {
    let sentinel = value.strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?=")).is_some();
    let safe = value.bytes().all(|byte| byte == b'\t' || (0x20..=0x7e).contains(&byte))
        && value.trim_matches([' ', '\t']) == value;
    if safe && !sentinel {
        value.to_string()
    } else {
        format!("=?base64?{}?=", base64::engine::general_purpose::STANDARD.encode(value))
    }
}

/// Build transport-owned headers. Callers must not append conflicting caller
/// headers (including Mcp-Param-*) after these have been generated.
pub fn request_headers(
    frame: &Value, version: &str, schema: Option<&Value>,
) -> Result<Vec<(String, String)>, String> {
    if !SUPPORTED.contains(&version) {
        return Err("unsupported MCP protocol version".into());
    }
    let mut headers = vec![("MCP-Protocol-Version".into(), version.into())];
    if version != MODERN { return Ok(headers); }
    let method = frame.get("method").and_then(Value::as_str)
        .filter(|method| !method.is_empty() && method.bytes().all(|byte| (0x21..=0x7e).contains(&byte)))
        .ok_or("invalid MCP request method")?;
    let meta = &frame["params"]["_meta"];
    if meta[VERSION_KEY].as_str() != Some(version) || !meta[CAPABILITIES_KEY].is_object() {
        return Err("missing or inconsistent MCP request metadata".into());
    }
    headers.push(("Mcp-Method".into(), method.into()));
    let name_key = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    if let Some(key) = name_key {
        let name = frame["params"][key].as_str().ok_or("missing MCP request name")?;
        headers.push(("Mcp-Name".into(), encode_header_value(name)));
    }
    if method == "tools/call" && let Some(schema) = schema {
        for annotation in header_annotations(schema)? {
            let mut value = &frame["params"]["arguments"];
            for property in &annotation.path {
                value = value.get(property).unwrap_or(&Value::Null);
            }
            if value.is_null() { continue; }
            let text = match annotation.kind.as_str() {
                "string" => value.as_str().map(str::to_string),
                "boolean" => value.as_bool().map(|value| value.to_string()),
                "integer" => integer_header(value),
                _ => None,
            }.ok_or("invalid value for MCP header parameter")?;
            headers.push((format!("Mcp-Param-{}", annotation.name), encode_header_value(&text)));
        }
    }
    Ok(headers)
}

fn integer_header(value: &Value) -> Option<String> {
    if let Some(integer) = value.as_i64() {
        return (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&integer)
            .then(|| integer.to_string());
    }
    // JSON Schema integers include numeric representations such as 42.0.
    let number = value.as_f64()?;
    if number.is_finite() && number.fract() == 0.0
        && number.abs() <= MAX_SAFE_INTEGER as f64
    {
        Some(format!("{number:.0}"))
    } else {
        None
    }
}

struct HeaderAnnotation {
    name: String,
    path: Vec<String>,
    kind: String,
}

/// Validate before exposing an HTTP tool. Invalid annotations reject the
/// whole tool; no network reference resolution is performed.
pub fn validate_header_schema(schema: &Value) -> Result<(), String> {
    header_annotations(schema).map(|_| ())
}

fn header_annotations(schema: &Value) -> Result<Vec<HeaderAnnotation>, String> {
    let mut annotations = Vec::new();
    let mut names = HashSet::new();
    let mut remaining = MAX_SCHEMA_NODES;
    walk_schema(schema, &mut Vec::new(), true, 0, &mut remaining, &mut names, &mut annotations)?;
    Ok(annotations)
}

fn header_token(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric()
        || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn walk_schema(
    schema: &Value, path: &mut Vec<String>, reachable: bool, depth: usize,
    remaining: &mut usize, names: &mut HashSet<String>, annotations: &mut Vec<HeaderAnnotation>,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH || *remaining == 0 {
        return Err("MCP header schema exceeds traversal limit".into());
    }
    *remaining -= 1;
    let Some(object) = schema.as_object() else { return Ok(()) };
    if let Some(annotation) = object.get("x-mcp-header") {
        let name = annotation.as_str().filter(|name| header_token(name))
            .ok_or("invalid x-mcp-header token")?;
        if !reachable || path.is_empty() {
            return Err("x-mcp-header is not reachable through properties".into());
        }
        let kind = object.get("type").and_then(Value::as_str)
            .filter(|kind| matches!(*kind, "string" | "integer" | "boolean"))
            .ok_or("x-mcp-header requires string, integer or boolean type")?;
        if !names.insert(name.to_ascii_lowercase()) {
            return Err("duplicate x-mcp-header name".into());
        }
        annotations.push(HeaderAnnotation { name: name.into(), path: path.clone(), kind: kind.into() });
    }
    for (keyword, child) in object {
        match keyword.as_str() {
            "properties" => {
                if let Some(properties) = child.as_object() {
                    for (property, subschema) in properties {
                        path.push(property.clone());
                        walk_schema(subschema, path, reachable, depth + 1, remaining, names, annotations)?;
                        path.pop();
                    }
                }
            }
            "$defs" | "definitions" | "patternProperties" | "dependentSchemas" => {
                if let Some(children) = child.as_object() {
                    for subschema in children.values() {
                        walk_schema(subschema, path, false, depth + 1, remaining, names, annotations)?;
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                if let Some(children) = child.as_array() {
                    for subschema in children {
                        walk_schema(subschema, path, false, depth + 1, remaining, names, annotations)?;
                    }
                }
            }
            "items" if child.is_array() => {
                for subschema in child.as_array().into_iter().flatten() {
                    walk_schema(subschema, path, false, depth + 1, remaining, names, annotations)?;
                }
            }
            "items" | "additionalItems" | "contains" | "additionalProperties"
            | "unevaluatedProperties" | "unevaluatedItems" | "propertyNames"
            | "not" | "if" | "then" | "else" | "contentSchema" => {
                walk_schema(child, path, false, depth + 1, remaining, names, annotations)?;
            }
            // Defaults, examples and enum/const values are instance data,
            // not schemas. Unknown keywords must not trigger dereferencing.
            _ => {}
        }
    }
    Ok(())
}

/// Tasks are not advertised by these clients. An absent discriminator remains
/// compatible with older servers; unknown discriminators never become success.
pub fn validate_result(frame: &Value) -> Result<(), String> {
    if frame.get("error").is_some() { return Ok(()); }
    let result = frame.get("result").filter(|result| result.is_object())
        .ok_or("missing MCP result object")?;
    match result.get("resultType") {
        None => Ok(()),
        Some(Value::String(kind)) if matches!(kind.as_str(), "complete" | "input_required") => Ok(()),
        _ => Err("unsupported MCP result type".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: Value) -> Value {
        let mut frame = json!({"id":1,"method":"tools/call","params":{"name":"工具","arguments":arguments}});
        inject_meta(&mut frame, MODERN, &json!({}));
        frame
    }

    #[test]
    fn versions_follow_client_preference_and_reject_unknown() {
        assert_eq!(best_supported(&json!(["2025-06-18", MODERN])), Some(MODERN));
        assert_eq!(best_supported(&json!(["2025-06-18", "2025-11-25"])), Some("2025-11-25"));
        assert_eq!(best_supported(&json!(["2099-01-01"])), None);
        assert_eq!(best_supported(&json!({})), None);
    }

    #[test]
    fn metadata_preserves_extension_and_replaces_peer_claims() {
        let mut frame = json!({"params":{"_meta":{"progressToken":"p","duduclaw_workflow":{"version":1},
            "io.modelcontextprotocol/protocolVersion":"fake"}}});
        inject_meta(&mut frame, MODERN, &json!({"elicitation":{"form":{}}}));
        assert_eq!(frame["params"]["_meta"][VERSION_KEY], MODERN);
        assert_eq!(frame["params"]["_meta"]["progressToken"], "p");
        assert_eq!(frame["params"]["_meta"]["duduclaw_workflow"]["version"], 1);
        let before = frame.clone();
        inject_meta(&mut frame, "2025-06-18", &json!({}));
        assert_eq!(frame, before);
    }

    #[test]
    fn header_encoding_preserves_ascii_and_escapes_ambiguous_values() {
        assert_eq!(encode_header_value("us-west1"), "us-west1");
        assert_eq!(encode_header_value("Hello, 世界"), "=?base64?SGVsbG8sIOS4lueVjA==?=");
        for value in [" padded ", "\tleading", "trailing\t", "line1\nline2", "=?base64?literal?=", "\u{7f}"] {
            let encoded = encode_header_value(value);
            let inner = encoded.strip_prefix("=?base64?").unwrap().strip_suffix("?=").unwrap();
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(inner).unwrap(), value.as_bytes());
        }
    }

    #[test]
    fn headers_mirror_exact_nested_paths_and_omit_null_or_absent() {
        let schema = json!({"properties":{"config":{"properties":{
            "tenant/key":{"type":"string","x-mcp-header":"Tenant"},
            "enabled":{"type":"boolean","x-mcp-header":"Enabled"},
            "count":{"type":"integer","x-mcp-header":"Count"},
            "empty":{"type":"string","x-mcp-header":"Empty"},
            "missing":{"type":"string","x-mcp-header":"Missing"}
        }}}});
        let frame = call(json!({"config":{"tenant/key":"台灣", "enabled":false,"count":-2,"empty":null}}));
        let headers = request_headers(&frame, MODERN, Some(&schema)).unwrap();
        let headers: std::collections::HashMap<_, _> = headers.into_iter().collect();
        assert_eq!(headers["MCP-Protocol-Version"], MODERN);
        assert_eq!(headers["Mcp-Method"], "tools/call");
        assert_eq!(headers["Mcp-Name"], encode_header_value("工具"));
        assert_eq!(headers["Mcp-Param-Tenant"], encode_header_value("台灣"));
        assert_eq!(headers["Mcp-Param-Enabled"], "false");
        assert_eq!(headers["Mcp-Param-Count"], "-2");
        assert!(!headers.contains_key("Mcp-Param-Empty"));
        assert!(!headers.contains_key("Mcp-Param-Missing"));
    }

    #[test]
    fn header_annotations_reject_invalid_names_types_and_locations() {
        for name in ["", "bad name", "x\r\ninjected", "非ASCII"] {
            assert!(validate_header_schema(&json!({"properties":{"p":{"type":"string","x-mcp-header":name}}})).is_err());
        }
        for kind in ["number", "object", "array", "null"] {
            assert!(validate_header_schema(&json!({"properties":{"p":{"type":kind,"x-mcp-header":"P"}}})).is_err());
        }
        let annotation = json!({"type":"string","x-mcp-header":"A"});
        assert!(validate_header_schema(&annotation).is_err());
        assert!(validate_header_schema(&json!({"properties":{"a":annotation,"b":{"type":"string","x-mcp-header":"a"}}})).is_err());
        for keyword in ["items", "not", "if", "then", "else", "additionalProperties"] {
            assert!(validate_header_schema(&json!({keyword:annotation})).is_err(), "{keyword}");
        }
        for keyword in ["oneOf", "anyOf", "allOf", "prefixItems"] {
            assert!(validate_header_schema(&json!({keyword:[{"properties":{"p":annotation}}]})).is_err(), "{keyword}");
        }
        assert!(validate_header_schema(&json!({"$defs":{"hidden":annotation},"$ref":"#/$defs/hidden"})).is_err());
        assert!(validate_header_schema(&json!({"examples":[annotation],"default":annotation,"const":annotation})).is_ok());
    }

    #[test]
    fn header_values_require_matching_primitives_and_safe_integers() {
        let schema = json!({"properties":{"n":{"type":"integer","x-mcp-header":"N"}}});
        for value in [json!(9_007_199_254_740_992_i64), json!(1.5), json!("12"), json!({})] {
            assert!(request_headers(&call(json!({"n":value})), MODERN, Some(&schema)).is_err());
        }
        assert!(request_headers(&call(json!({"n":MAX_SAFE_INTEGER})), MODERN, Some(&schema)).is_ok());
        let numeric = request_headers(&call(json!({"n":42.0})), MODERN, Some(&schema)).unwrap();
        assert!(numeric.contains(&("Mcp-Param-N".into(), "42".into())));
        assert_eq!(request_headers(&json!({}), "2025-06-18", None).unwrap().len(), 1);
        assert!(request_headers(&json!({"method":"tools/list"}), MODERN, None).is_err());
    }

    #[test]
    fn schema_traversal_is_bounded() {
        let mut schema = json!({"type":"string","x-mcp-header":"A"});
        for _ in 0..MAX_SCHEMA_DEPTH + 1 { schema = json!({"properties":{"nested":schema}}); }
        assert!(validate_header_schema(&schema).is_err());
    }

    #[test]
    fn result_discriminators_fail_closed() {
        for result in [json!({}), json!({"resultType":"complete"}), json!({"resultType":"input_required"})] {
            assert!(validate_result(&json!({"result":result})).is_ok());
        }
        for result in [json!({"resultType":"task"}), json!({"resultType":"future"}), json!({"resultType":null}), json!([])] {
            assert!(validate_result(&json!({"result":result})).is_err());
        }
        assert!(validate_result(&json!({"error":{"code":-32022}})).is_ok());
    }
}
