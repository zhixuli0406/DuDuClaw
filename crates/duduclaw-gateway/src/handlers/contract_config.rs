//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Convert a JSON value into a TOML value for generic pass-through sections.
/// Returns None for null / unrepresentable values.
pub(crate) fn json_to_toml(v: &Value) -> Option<toml::Value> {
    match v {
        Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        Value::String(s) => Some(toml::Value::String(s.clone())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for item in a {
                out.push(json_to_toml(item)?);
            }
            Some(toml::Value::Array(out))
        }
        Value::Object(o) => {
            let mut m = toml::map::Map::new();
            for (k, val) in o {
                m.insert(k.clone(), json_to_toml(val)?);
            }
            Some(toml::Value::Table(m))
        }
        Value::Null => None,
    }
}

/// Build the `[boundaries]` table for a CONTRACT.toml from `contract.update`
/// params. Validates `max_tool_calls_per_turn` range. Returns the full table to
/// serialise (the contract file only contains `[boundaries]`).
pub(crate) fn build_contract_table(params: &Value) -> Result<toml::Table, String> {
    fn string_array(params: &Value, key: &str) -> Result<Vec<toml::Value>, String> {
        let arr = params
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("Missing or invalid '{key}' (expected array of strings)"))?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item
                .as_str()
                .ok_or_else(|| format!("'{key}' entries must be strings"))?;
            let s = s.trim();
            if s.is_empty() {
                return Err(format!("'{key}' entries must be non-empty"));
            }
            out.push(toml::Value::String(s.into()));
        }
        Ok(out)
    }

    let must_not = string_array(params, "must_not")?;
    let must_always = string_array(params, "must_always")?;
    let max_calls = params
        .get("max_tool_calls_per_turn")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if max_calls > 1000 {
        return Err("max_tool_calls_per_turn must be 0-1000 (0 = unlimited)".into());
    }

    let mut boundaries = toml::map::Map::new();
    boundaries.insert("must_not".into(), toml::Value::Array(must_not));
    boundaries.insert("must_always".into(), toml::Value::Array(must_always));
    boundaries.insert(
        "max_tool_calls_per_turn".into(),
        toml::Value::Integer(max_calls as i64),
    );

    let mut table = toml::Table::new();
    table.insert("boundaries".into(), toml::Value::Table(boundaries));
    Ok(table)
}

/// Parse a CONTRACT.toml table into the `contract.get` response shape.
pub(crate) fn contract_table_to_response(table: &toml::Table) -> Value {
    let boundaries = table.get("boundaries").and_then(|v| v.as_table());
    let str_arr = |key: &str| -> Vec<String> {
        boundaries
            .and_then(|b| b.get(key))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let max_calls = boundaries
        .and_then(|b| b.get("max_tool_calls_per_turn"))
        .and_then(|v| v.as_integer())
        .unwrap_or(0);
    json!({
        "must_not": str_arr("must_not"),
        "must_always": str_arr("must_always"),
        "max_tool_calls_per_turn": max_calls,
    })
}
