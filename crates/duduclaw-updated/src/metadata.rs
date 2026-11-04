//! Canonical metadata: compact UTF-8 JSON, lexicographic object keys, no floats.
//! This defines package F's signed document representation, not a contract change.
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use crate::{Error, Result};

pub const MAX_METADATA_BYTES: usize = 256 * 1024;

pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    fn write(value: &Value, out: &mut Vec<u8>) -> Result<()> {
        match value {
            Value::Object(fields) => {
                out.push(b'{');
                let mut keys: Vec<_> = fields.keys().collect();
                keys.sort();
                for (index, key) in keys.into_iter().enumerate() {
                    if index != 0 { out.push(b','); }
                    serde_json::to_writer(&mut *out, key)?;
                    out.push(b':');
                    write(&fields[key], out)?;
                }
                out.push(b'}');
            }
            Value::Array(values) => {
                out.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 { out.push(b','); }
                    write(value, out)?;
                }
                out.push(b']');
            }
            Value::Number(number) if !number.is_u64() && !number.is_i64() => return Err(Error::Invalid),
            other => serde_json::to_writer(&mut *out, other)?,
        }
        if out.len() > MAX_METADATA_BYTES { return Err(Error::Invalid); }
        Ok(())
    }
    let mut bytes = Vec::new();
    write(&serde_json::to_value(value)?, &mut bytes)?;
    Ok(bytes)
}

pub fn decode_canonical<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T> {
    if bytes.is_empty() || bytes.len() > MAX_METADATA_BYTES { return Err(Error::Invalid); }
    let value: T = serde_json::from_slice(bytes)?;
    if canonical_bytes(&value)? != bytes { return Err(Error::Invalid); }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_metadata_rejects_ambiguity() {
        let value = serde_json::json!({"z": [{"b": 2, "a": "員工"}], "a": 1});
        let bytes = canonical_bytes(&value).unwrap();
        assert_eq!(bytes, "{\"a\":1,\"z\":[{\"a\":\"員工\",\"b\":2}]}".as_bytes());
        assert_eq!(decode_canonical::<Value>(&bytes).unwrap(), value);
        for bad in [r#"{"b":2,"a":1}"#, r#"{"a":1,"a":1}"#, r#"{ "a":1}"#,
            r#"{"a":1.0}"#, "{\"a\":1}\n", r#"{"a":"\u0061"}"#] {
            assert!(decode_canonical::<Value>(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(decode_canonical::<Value>(&vec![b' '; MAX_METADATA_BYTES + 1]).is_err());
    }
}
