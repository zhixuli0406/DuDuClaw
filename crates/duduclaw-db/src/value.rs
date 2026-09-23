//! Row → JSON value mapping (§13.7 "值型別對映").
//!
//! Integers and floats become JSON numbers, booleans stay booleans, SQL NULL
//! becomes JSON null, text becomes a string, dates/timestamps become ISO-8601
//! strings, `NUMERIC`/`DECIMAL` becomes a **string** (never a float — that is
//! how money loses cents), `bytea`/`BLOB` becomes base64, and JSON columns
//! come back parsed rather than double-encoded.
//!
//! Each driver gets its own mapper keyed on the column's own type name. The
//! `Any` driver would have collapsed all three into one function but it decodes
//! only the lowest common denominator (no decimals, no JSON, no timestamps),
//! which is precisely the mapping this module exists to provide.
//!
//! A type nothing here knows how to decode is reported as
//! `"<unsupported type: NAME>"` rather than guessed at or silently dropped —
//! an honest placeholder beats a wrong value, and a dropped key would make a
//! redaction rule targeting that column silently match nothing.

use base64::Engine as _;
use serde_json::{Map, Value};
use sqlx::postgres::PgValueFormat;
use sqlx::{Column, Row, TypeInfo, ValueRef};

/// JSON object for one row.
pub type JsonRow = Map<String, Value>;

fn f64_to_json(v: f64) -> Value {
    match serde_json::Number::from_f64(v) {
        Some(n) => Value::Number(n),
        // NaN / ±Infinity have no JSON number form. Stringifying keeps the
        // information instead of turning it into a misleading `null`.
        None => Value::String(v.to_string()),
    }
}

fn b64(bytes: &[u8]) -> Value {
    Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn unsupported(type_name: &str) -> Value {
    Value::String(format!("<unsupported type: {type_name}>"))
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────

/// Map one `PgRow` to a JSON object.
pub fn pg_row_to_json(row: &sqlx::postgres::PgRow) -> JsonRow {
    let mut out = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let type_name = col.type_info().name().to_ascii_uppercase();
        let is_null = row
            .try_get_raw(i)
            .map(|v| v.is_null())
            .unwrap_or(true);
        let value = if is_null {
            Value::Null
        } else {
            pg_value(row, i, &type_name)
        };
        out.insert(col.name().to_string(), value);
    }
    out
}

fn pg_value(row: &sqlx::postgres::PgRow, i: usize, type_name: &str) -> Value {
    use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

    match type_name {
        "BOOL" => row.try_get::<bool, _>(i).map(Value::Bool).unwrap_or(Value::Null),
        "INT2" => row.try_get::<i16, _>(i).map(|v| Value::from(v as i64)).unwrap_or(Value::Null),
        "INT4" => row.try_get::<i32, _>(i).map(|v| Value::from(v as i64)).unwrap_or(Value::Null),
        "INT8" => row.try_get::<i64, _>(i).map(Value::from).unwrap_or(Value::Null),
        "FLOAT4" => row
            .try_get::<f32, _>(i)
            .map(|v| f64_to_json(v as f64))
            .unwrap_or(Value::Null),
        "FLOAT8" => row.try_get::<f64, _>(i).map(f64_to_json).unwrap_or(Value::Null),
        "NUMERIC" => pg_numeric_text(row, i)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "TEXT" | "VARCHAR" | "BPCHAR" | "CHAR" | "NAME" | "CITEXT" | "UNKNOWN" | "INET"
        | "CIDR" | "MACADDR" | "XML" => {
            row.try_get::<String, _>(i).map(Value::String).unwrap_or(Value::Null)
        }
        "TIMESTAMPTZ" => row
            .try_get::<DateTime<Utc>, _>(i)
            .map(|v| Value::String(v.to_rfc3339()))
            .unwrap_or(Value::Null),
        "TIMESTAMP" => row
            .try_get::<NaiveDateTime, _>(i)
            .map(|v| Value::String(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string()))
            .unwrap_or(Value::Null),
        "DATE" => row
            .try_get::<NaiveDate, _>(i)
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null),
        "TIME" => row
            .try_get::<NaiveTime, _>(i)
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null),
        "UUID" => row
            .try_get::<sqlx::types::Uuid, _>(i)
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null),
        "JSON" | "JSONB" => row.try_get::<Value, _>(i).unwrap_or(Value::Null),
        "BYTEA" => row
            .try_get::<Vec<u8>, _>(i)
            .map(|v| b64(&v))
            .unwrap_or(Value::Null),
        other => row
            .try_get::<String, _>(i)
            .map(Value::String)
            .unwrap_or_else(|_| unsupported(other)),
    }
}

/// PostgreSQL `NUMERIC` as the exact text PostgreSQL itself would print.
///
/// Going through `BigDecimal` loses the column's display scale: sqlx's
/// `PgNumeric -> BigDecimal` conversion derives the scale from the count of
/// base-10000 digit groups (`(digits.len() - weight - 1) * 4`) and discards
/// the `dscale` the server actually sent. A `numeric(10,2)` holding `12.50`
/// comes back as `"12.5000"`, and `0.00` as `"0"` — the *value* is right, the
/// money is unreadable. `PgNumeric` is private in sqlx 0.8, so the wire form is
/// decoded here instead. `BigDecimal` remains the fallback: a parse failure
/// costs presentation, never the value.
fn pg_numeric_text(row: &sqlx::postgres::PgRow, i: usize) -> Option<String> {
    if let Ok(raw) = row.try_get_raw(i) {
        let format = raw.format();
        if let Ok(bytes) = raw.as_bytes() {
            match format {
                // Simple-query results (any `db_query` without binds) arrive as
                // text — already exactly what `psql` would show.
                PgValueFormat::Text => {
                    if let Ok(text) = std::str::from_utf8(bytes) {
                        return Some(text.to_string());
                    }
                }
                PgValueFormat::Binary => {
                    if let Some(text) = decode_pg_numeric_binary(bytes) {
                        return Some(text);
                    }
                }
            }
        }
    }
    row.try_get::<sqlx::types::BigDecimal, _>(i)
        .ok()
        .map(|v| v.to_string())
}

/// PostgreSQL's binary `numeric` wire format:
/// `int16 ndigits`, `int16 weight`, `uint16 sign`, `uint16 dscale`, then
/// `ndigits` base-10000 digit groups. Rendering mirrors the server's own
/// `get_str_from_var`, so the string matches what PostgreSQL prints.
fn decode_pg_numeric_binary(b: &[u8]) -> Option<String> {
    if b.len() < 8 {
        return None;
    }
    let ndigits = i16::from_be_bytes([b[0], b[1]]);
    let weight = i16::from_be_bytes([b[2], b[3]]);
    let sign = u16::from_be_bytes([b[4], b[5]]);
    let dscale = u16::from_be_bytes([b[6], b[7]]) as usize;

    match sign {
        0xC000 => return Some("NaN".to_string()),
        0xD000 => return Some("Infinity".to_string()),
        0xF000 => return Some("-Infinity".to_string()),
        0x0000 | 0x4000 => {}
        _ => return None,
    }
    if ndigits < 0 {
        return None;
    }
    let ndigits = ndigits as usize;
    if b.len() < 8 + ndigits * 2 {
        return None;
    }
    let digits: Vec<i16> = (0..ndigits)
        .map(|k| i16::from_be_bytes([b[8 + k * 2], b[9 + k * 2]]))
        .collect();
    if digits.iter().any(|&d| !(0..10_000).contains(&d)) {
        return None;
    }

    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    if weight < 0 {
        out.push('0');
    } else {
        for d in 0..=(weight as usize) {
            let v = digits.get(d).copied().unwrap_or(0);
            if d == 0 {
                out.push_str(&v.to_string());
            } else {
                out.push_str(&format!("{v:04}"));
            }
        }
    }
    if dscale > 0 {
        out.push('.');
        let mut frac = String::new();
        let mut idx = weight as i64 + 1;
        while frac.chars().count() < dscale {
            let v = if idx < 0 {
                0
            } else {
                digits.get(idx as usize).copied().unwrap_or(0)
            };
            frac.push_str(&format!("{v:04}"));
            idx += 1;
        }
        // All ASCII by construction, but counted by char to keep the
        // no-raw-byte-slicing rule unconditional.
        out.extend(frac.chars().take(dscale));
    }
    Some(out)
}

// ── MySQL ───────────────────────────────────────────────────────────────────

/// Map one `MySqlRow` to a JSON object.
pub fn mysql_row_to_json(row: &sqlx::mysql::MySqlRow) -> JsonRow {
    let mut out = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let type_name = col.type_info().name().to_ascii_uppercase();
        let is_null = row
            .try_get_raw(i)
            .map(|v| v.is_null())
            .unwrap_or(true);
        let value = if is_null {
            Value::Null
        } else {
            mysql_value(row, i, &type_name)
        };
        out.insert(col.name().to_string(), value);
    }
    out
}

fn mysql_value(row: &sqlx::mysql::MySqlRow, i: usize, type_name: &str) -> Value {
    use sqlx::types::chrono::{NaiveDate, NaiveDateTime, NaiveTime};

    // `INT UNSIGNED`, `BIGINT UNSIGNED`, … — the width prefix is what decides
    // the decoder, the suffix only decides signedness.
    let unsigned = type_name.ends_with(" UNSIGNED");
    let base = type_name.trim_end_matches(" UNSIGNED");

    match base {
        "BOOLEAN" => row.try_get::<bool, _>(i).map(Value::Bool).unwrap_or(Value::Null),
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => {
            if unsigned {
                row.try_get::<u64, _>(i)
                    .map(Value::from)
                    .unwrap_or(Value::Null)
            } else {
                row.try_get::<i64, _>(i)
                    .map(Value::from)
                    .unwrap_or(Value::Null)
            }
        }
        "FLOAT" => row
            .try_get::<f32, _>(i)
            .map(|v| f64_to_json(v as f64))
            .unwrap_or(Value::Null),
        "DOUBLE" => row.try_get::<f64, _>(i).map(f64_to_json).unwrap_or(Value::Null),
        "DECIMAL" | "NEWDECIMAL" => row
            .try_get::<sqlx::types::BigDecimal, _>(i)
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null),
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            row.try_get::<String, _>(i).map(Value::String).unwrap_or(Value::Null)
        }
        "DATETIME" | "TIMESTAMP" => row
            .try_get::<NaiveDateTime, _>(i)
            .map(|v| Value::String(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string()))
            .unwrap_or(Value::Null),
        "DATE" => row
            .try_get::<NaiveDate, _>(i)
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null),
        "TIME" => row
            .try_get::<NaiveTime, _>(i)
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null),
        "JSON" => row.try_get::<Value, _>(i).unwrap_or(Value::Null),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" | "BIT"
        | "GEOMETRY" => row
            .try_get::<Vec<u8>, _>(i)
            .map(|v| b64(&v))
            .unwrap_or(Value::Null),
        other => row
            .try_get::<String, _>(i)
            .map(Value::String)
            .unwrap_or_else(|_| unsupported(other)),
    }
}

// ── SQLite ──────────────────────────────────────────────────────────────────

/// Map one `SqliteRow` to a JSON object.
///
/// SQLite is dynamically typed: the declared column type is a hint, the
/// *stored* value's storage class is the truth. The storage class from the
/// value ref is therefore what drives the mapping (so a `BOOLEAN` column
/// holding `1` comes back as the number `1`, which is what is actually there).
pub fn sqlite_row_to_json(row: &sqlx::sqlite::SqliteRow) -> JsonRow {
    let mut out = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let value = match row.try_get_raw(i) {
            Ok(raw) => {
                if raw.is_null() {
                    Value::Null
                } else {
                    let storage = raw.type_info().name().to_ascii_uppercase();
                    sqlite_value(row, i, &storage)
                }
            }
            Err(_) => Value::Null,
        };
        out.insert(col.name().to_string(), value);
    }
    out
}

fn sqlite_value(row: &sqlx::sqlite::SqliteRow, i: usize, storage: &str) -> Value {
    match storage {
        "NULL" => Value::Null,
        "INTEGER" | "INT" | "BIGINT" | "BOOLEAN" => {
            row.try_get::<i64, _>(i).map(Value::from).unwrap_or(Value::Null)
        }
        "REAL" | "FLOAT" | "DOUBLE" | "NUMERIC" => {
            row.try_get::<f64, _>(i).map(f64_to_json).unwrap_or(Value::Null)
        }
        "TEXT" | "VARCHAR" | "DATETIME" | "DATE" | "TIME" => {
            row.try_get::<String, _>(i).map(Value::String).unwrap_or(Value::Null)
        }
        "BLOB" => row
            .try_get::<Vec<u8>, _>(i)
            .map(|v| b64(&v))
            .unwrap_or(Value::Null),
        other => row
            .try_get::<String, _>(i)
            .map(Value::String)
            .unwrap_or_else(|_| unsupported(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_floats_become_numbers() {
        assert_eq!(f64_to_json(1.5), Value::from(1.5));
        assert_eq!(f64_to_json(0.0), Value::from(0.0));
    }

    #[test]
    fn non_finite_floats_become_strings_not_null() {
        assert_eq!(f64_to_json(f64::NAN), Value::String("NaN".into()));
        assert_eq!(f64_to_json(f64::INFINITY), Value::String("inf".into()));
    }

    #[test]
    fn bytes_become_base64() {
        assert_eq!(b64(b"hi"), Value::String("aGk=".into()));
        assert_eq!(b64(&[]), Value::String(String::new()));
    }

    /// Synthetic wire images, so the renderer is locked without a server.
    /// Layout: ndigits, weight, sign, dscale, then base-10000 groups.
    #[test]
    fn pg_binary_numeric_renders_like_postgres() {
        fn img(weight: i16, sign: u16, dscale: u16, digits: &[i16]) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&(digits.len() as i16).to_be_bytes());
            v.extend_from_slice(&weight.to_be_bytes());
            v.extend_from_slice(&sign.to_be_bytes());
            v.extend_from_slice(&dscale.to_be_bytes());
            for d in digits {
                v.extend_from_slice(&d.to_be_bytes());
            }
            v
        }
        // numeric(10,2) = 12.50 — the case BigDecimal renders as "12.5000".
        assert_eq!(
            decode_pg_numeric_binary(&img(0, 0x0000, 2, &[12, 5000])).as_deref(),
            Some("12.50")
        );
        // numeric(10,2) = 0.00 — BigDecimal renders this as a bare "0".
        assert_eq!(
            decode_pg_numeric_binary(&img(0, 0x0000, 2, &[])).as_deref(),
            Some("0.00")
        );
        assert_eq!(
            decode_pg_numeric_binary(&img(0, 0x4000, 2, &[7, 2500])).as_deref(),
            Some("-7.25")
        );
        // Unconstrained numeric 1234567.891 → groups 123|4567|8910, dscale 3.
        assert_eq!(
            decode_pg_numeric_binary(&img(1, 0x0000, 3, &[123, 4567, 8910])).as_deref(),
            Some("1234567.891")
        );
        // Integers keep no decimal point.
        assert_eq!(
            decode_pg_numeric_binary(&img(0, 0x0000, 0, &[42])).as_deref(),
            Some("42")
        );
        // Value smaller than one group: 0.0001 → weight -1.
        assert_eq!(
            decode_pg_numeric_binary(&img(-1, 0x0000, 4, &[1])).as_deref(),
            Some("0.0001")
        );
        assert_eq!(
            decode_pg_numeric_binary(&img(0, 0xC000, 0, &[])).as_deref(),
            Some("NaN")
        );
        // Malformed images fall through to the BigDecimal path.
        assert_eq!(decode_pg_numeric_binary(&[0, 1]), None);
        assert_eq!(decode_pg_numeric_binary(&img(0, 0x1234, 0, &[1])), None);
        let mut truncated = img(0, 0x0000, 2, &[12, 5000]);
        truncated.truncate(9);
        assert_eq!(decode_pg_numeric_binary(&truncated), None);
    }

    #[test]
    fn unsupported_is_labelled_not_guessed() {
        assert_eq!(
            unsupported("TSVECTOR"),
            Value::String("<unsupported type: TSVECTOR>".into())
        );
    }
}
