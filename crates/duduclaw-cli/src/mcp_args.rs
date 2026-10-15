//! Type-tolerant readers for MCP tool arguments.
//!
//! `tools/list` advertises integers, numbers, booleans and string arrays with
//! their real JSON types, but some runtimes stringify every tool argument
//! (`"5"`, `"true"`, `"a,b"`). Each reader accepts both spellings so the
//! advertised schema and the handler agree whichever one arrives. A value of
//! the wrong shape reads as absent, which lets the caller apply its default.

use serde_json::Value;

/// A non-negative integer: a JSON number or a numeric string. An integral
/// float (`10.0`, `"10.0"`, `1e3`) counts as that integer — some clients
/// serialize every number as a float. A fractional, negative, non-finite or
/// out-of-range value reads as absent. Callers still clamp the result.
pub(crate) fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    let from_float = |f: f64| {
        // 2^64 is exactly representable; anything at or above it overflows.
        (f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f < 18_446_744_073_709_551_616.0)
            .then_some(f as u64)
    };
    match args.get(key)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().and_then(from_float)),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<u64>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().and_then(from_float))
        }
        _ => None,
    }
}

/// A finite float: a JSON number or a numeric string (`"NaN"` / `"inf"` read
/// as absent).
pub(crate) fn arg_f64(args: &Value, key: &str) -> Option<f64> {
    let v = match args.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }?;
    v.is_finite().then_some(v)
}

/// A boolean: JSON `true`/`false` or the strings `true`/`false`/`1`/`0`/`yes`/`no`.
pub(crate) fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    match args.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A list of strings: a JSON array of strings, a string holding a JSON array,
/// or a comma-separated string. Entries are trimmed; empty ones are dropped.
pub(crate) fn arg_string_list(args: &Value, key: &str) -> Option<Vec<String>> {
    let from_array = |arr: &[Value]| -> Vec<String> {
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    match args.get(key)? {
        Value::Array(arr) => Some(from_array(arr)),
        Value::String(s) => {
            let s = s.trim();
            if s.starts_with('[') {
                match serde_json::from_str::<Value>(s) {
                    Ok(Value::Array(arr)) => return Some(from_array(&arr)),
                    _ => return None,
                }
            }
            Some(
                s.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_accept_native_and_string_forms() {
        let a = json!({"n": 5, "s": " 7 ", "neg": -1, "bad": "x", "f": 0.25, "fs": "0.5"});
        assert_eq!(arg_u64(&a, "n"), Some(5));
        assert_eq!(arg_u64(&a, "s"), Some(7));
        assert_eq!(arg_u64(&a, "neg"), None);
        assert_eq!(arg_u64(&a, "bad"), None);
        assert_eq!(arg_u64(&a, "missing"), None);
        assert_eq!(arg_f64(&a, "f"), Some(0.25));
        assert_eq!(arg_f64(&a, "fs"), Some(0.5));
        assert_eq!(arg_f64(&a, "n"), Some(5.0));
        // Integral floats are integers; fractional / negative ones are absent.
        let fl = json!({"f": 10.0, "fs": "10.0", "e": 1e3, "frac": 2.5, "nf": -3.0, "big": 1e300});
        assert_eq!(arg_u64(&fl, "f"), Some(10));
        assert_eq!(arg_u64(&fl, "fs"), Some(10));
        assert_eq!(arg_u64(&fl, "e"), Some(1000));
        assert_eq!(arg_u64(&fl, "frac"), None);
        assert_eq!(arg_u64(&fl, "nf"), None);
        assert_eq!(arg_u64(&fl, "big"), None);
        assert_eq!(arg_u64(&json!({"s": "NaN"}), "s"), None);
        let odd = json!({"nan": "NaN", "inf": "inf"});
        assert_eq!(arg_f64(&odd, "nan"), None);
        assert_eq!(arg_f64(&odd, "inf"), None);
    }

    #[test]
    fn booleans_accept_native_and_string_forms() {
        let a = json!({"t": true, "f": false, "st": "TRUE", "sf": "false", "z": "0", "x": "maybe", "n": 1});
        assert_eq!(arg_bool(&a, "t"), Some(true));
        assert_eq!(arg_bool(&a, "f"), Some(false));
        assert_eq!(arg_bool(&a, "st"), Some(true));
        assert_eq!(arg_bool(&a, "sf"), Some(false));
        assert_eq!(arg_bool(&a, "z"), Some(false));
        assert_eq!(arg_bool(&a, "x"), None);
        assert_eq!(arg_bool(&a, "n"), None);
    }

    #[test]
    fn string_lists_accept_array_json_string_and_csv() {
        let a = json!({
            "arr": [" a ", "", "b", 3],
            "json": "[\"x\", \"y\"]",
            "csv": "p, q,,r",
            "broken": "[\"x\"",
            "num": 4
        });
        assert_eq!(
            arg_string_list(&a, "arr"),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            arg_string_list(&a, "json"),
            Some(vec!["x".into(), "y".into()])
        );
        assert_eq!(
            arg_string_list(&a, "csv"),
            Some(vec!["p".into(), "q".into(), "r".into()])
        );
        assert_eq!(arg_string_list(&a, "broken"), None);
        assert_eq!(arg_string_list(&a, "num"), None);
        assert_eq!(arg_string_list(&a, "missing"), None);
    }
}
