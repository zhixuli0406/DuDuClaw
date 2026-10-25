//! Private bounded array-element adapter over one owned, fully validated JSON
//! input. It is storage byte preservation, not an authority or a JSON parser.
use super::{Error, RawDigest, Result};
use serde::de::IgnoredAny;
use serde_json::Value;
use std::{io::Read, ops::Range, path::Path};
use zeroize::Zeroizing;
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_ROWS: usize = 512;
pub(crate) struct RawArray {
    bytes: Option<Zeroizing<Vec<u8>>>,
    values: Vec<Value>,
    ranges: Vec<Range<usize>>,
}
pub(crate) fn valid_provider(provider: &str) -> bool {
    !provider.is_empty() && provider.len() <= 128 && !provider.chars().any(char::is_control)
}
fn validate_rows(rows: &[Value]) -> Result<()> {
    if rows.len() > MAX_ROWS {
        return Err(Error::Invalid);
    }
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        let id = row
            .get("provider_id")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?;
        if !row.is_object() || !valid_provider(id) || !ids.insert(id) {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}
impl RawArray {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        match std::fs::symlink_metadata(path) {
            Ok(m) if !m.is_file() || m.file_type().is_symlink() => return Err(Error::Invalid),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::from_bytes(None),
            Err(_) => return Err(Error::Io),
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(path).map_err(|_| Error::Io)?;
        let metadata = file.metadata().map_err(|_| Error::Io)?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
            return Err(Error::Invalid);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        std::io::Read::by_ref(&mut file)
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Io)?;
        Self::from_owned(Some(bytes))
    }
    pub(crate) fn from_bytes(bytes: Option<Vec<u8>>) -> Result<Self> {
        Self::from_owned(bytes.map(Zeroizing::new))
    }
    fn from_owned(bytes: Option<Zeroizing<Vec<u8>>>) -> Result<Self> {
        let Some(raw) = bytes.as_ref() else {
            return Ok(Self {
                bytes: None,
                values: vec![],
                ranges: vec![],
            });
        };
        if raw.len() > MAX_BYTES {
            return Err(Error::Invalid);
        }
        // The complete document is parsed first. Streaming below only locates
        // already-validated element boundaries in this very same input.
        let values: Vec<Value> = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
        validate_rows(&values)?;
        let mut pos = 0usize;
        fn ws(raw: &[u8], pos: &mut usize) {
            while raw.get(*pos).is_some_and(u8::is_ascii_whitespace) {
                *pos += 1;
            }
        }
        ws(raw, &mut pos);
        if raw.get(pos) != Some(&b'[') {
            return Err(Error::Invalid);
        }
        pos += 1;
        let mut ranges = Vec::new();
        for (i, value) in values.iter().enumerate() {
            ws(raw, &mut pos);
            if i > 0 {
                if raw.get(pos) != Some(&b',') {
                    return Err(Error::Invalid);
                }
                pos += 1;
                ws(raw, &mut pos);
            }
            let start = pos;
            let mut stream =
                serde_json::Deserializer::from_slice(&raw[start..]).into_iter::<IgnoredAny>();
            stream
                .next()
                .ok_or(Error::Invalid)?
                .map_err(|_| Error::Invalid)?;
            let consumed = stream.byte_offset();
            if consumed == 0 {
                return Err(Error::Invalid);
            }
            pos = start.checked_add(consumed).ok_or(Error::Invalid)?;
            let mut end = pos;
            while end > start && raw[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
            if serde_json::from_slice::<Value>(&raw[start..end]).map_err(|_| Error::Invalid)?
                != *value
            {
                return Err(Error::Invalid);
            }
            // A derived struct rejects a repeated provider_id key even though
            // Value itself would accept its final occurrence. Unknown fields
            // remain opaque in the original element, not a generic schema.
            #[derive(serde::Deserialize)]
            struct ProviderIdentity {
                provider_id: String,
            }
            let identity: ProviderIdentity =
                serde_json::from_slice(&raw[start..end]).map_err(|_| Error::Invalid)?;
            if value["provider_id"] != identity.provider_id {
                return Err(Error::Invalid);
            }
            ranges.push(start..end);
        }
        ws(raw, &mut pos);
        if raw.get(pos) != Some(&b']') {
            return Err(Error::Invalid);
        }
        pos += 1;
        ws(raw, &mut pos);
        if pos != raw.len() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            bytes,
            values,
            ranges,
        })
    }
    pub(crate) fn values(&self) -> &[Value] {
        &self.values
    }
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_ref().map(|v| v.as_slice())
    }
    pub(crate) fn byte_digest(&self) -> Option<RawDigest> {
        self.bytes().map(RawDigest::of)
    }
    pub(crate) fn same_bytes(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
    pub(crate) fn element(&self, provider: &str) -> Option<&[u8]> {
        let index = self
            .values
            .iter()
            .position(|v| v["provider_id"] == provider)?;
        Some(&self.bytes.as_ref()?[self.ranges[index].clone()])
    }
    pub(crate) fn render(&self, next: &[Value]) -> Result<Zeroizing<Vec<u8>>> {
        validate_rows(next)?;
        if next == self.values.as_slice() {
            if let Some(bytes) = self.bytes() {
                return Ok(Zeroizing::new(bytes.to_vec()));
            }
        }
        let mut output = Zeroizing::new(b"[\n".to_vec());
        for (i, row) in next.iter().enumerate() {
            if i > 0 {
                output.extend_from_slice(b",\n");
            }
            let provider = row["provider_id"].as_str().ok_or(Error::Invalid)?;
            if self.values.iter().any(|old| old == row) {
                output.extend_from_slice(self.element(provider).ok_or(Error::Invalid)?);
            } else {
                let encoded =
                    Zeroizing::new(serde_json::to_vec_pretty(row).map_err(|_| Error::Invalid)?);
                output.extend_from_slice(&encoded);
            }
            if output.len() > MAX_BYTES.saturating_sub(3) {
                return Err(Error::Invalid);
            }
        }
        if !next.is_empty() {
            output.push(b'\n');
        }
        output.extend_from_slice(b"]\n");
        if output.len() > MAX_BYTES {
            return Err(Error::Invalid);
        }
        // Independent complete serde parse, then ranges from the emitted owned
        // bytes. Every unchanged row must really retain its original slice.
        let checked = Self::from_bytes(Some(output.to_vec()))?;
        if checked.values() != next {
            return Err(Error::Invalid);
        }
        for row in next {
            if self.values.iter().any(|old| old == row) {
                let p = row["provider_id"].as_str().ok_or(Error::Invalid)?;
                if self.element(p) != checked.element(p) {
                    return Err(Error::Invalid);
                }
            }
        }
        Ok(output)
    }
}
