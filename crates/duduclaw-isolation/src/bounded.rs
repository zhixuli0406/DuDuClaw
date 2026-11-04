//! Bounded wire collections for untrusted messages (§4, §6.2, §10.2).
use serde::{Deserialize, Deserializer, Serialize};
use crate::ContractError;

/// UTF-8 text with a byte limit, enforced on construction and deserialization (§6.2).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Text<const MAX: usize>(String);

impl<const MAX: usize> Text<MAX> {
    /// Check the UTF-8 byte length; no truncation or normalization is performed.
    pub fn new(value: String) -> Result<Self, ContractError> {
        if value.len() > MAX { return Err(ContractError::LimitExceeded("text")); }
        Ok(Self(value))
    }

    /// Borrow the complete text.
    pub fn as_str(&self) -> &str { &self.0 }
}

impl<const MAX: usize> std::fmt::Debug for Text<MAX> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Text").field("bytes", &self.0.len()).finish()
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for Text<MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// A collection with an enforced item count (§4, §6.2); outer framing bounds bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct List<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> List<T, MAX> {
    /// Reject excess entries instead of silently dropping them.
    pub fn new(value: Vec<T>) -> Result<Self, ContractError> {
        if value.len() > MAX { return Err(ContractError::LimitExceeded("list")); }
        Ok(Self(value))
    }

    /// Borrow entries in wire order.
    pub fn as_slice(&self) -> &[T] { &self.0 }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for List<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor<T, const N: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Visitor<T, N> {
            type Value = List<T, N>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "at most {N} entries")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    if items.len() == N { return Err(serde::de::Error::custom("too many entries")); }
                    items.push(item);
                }
                Ok(List(items))
            }
        }
        d.deserialize_seq(Visitor::<T, MAX>(std::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_apply_to_utf8_bytes_and_deserialization() {
        assert!(Text::<3>::new("員".into()).is_ok());
        assert!(serde_json::from_str::<Text<2>>("\"員\"").is_err());
        assert!(serde_json::from_str::<List<u8, 1>>("[1,2]").is_err());
        assert!(List::<u8, 1>::new(vec![1, 2]).is_err());
    }
}
