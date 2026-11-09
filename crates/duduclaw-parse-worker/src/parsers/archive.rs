use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use duduclaw_isolation::worker::ParseError;

pub(crate) const MAX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 1000;
const MAX_DEPTH: usize = 32;
const MAX_RATIO: u64 = 100;
pub(crate) const MAX_XML_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) struct Summary {
    pub entries: u32,
    pub expanded_bytes: u64,
}

pub(crate) fn open(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, ParseError> {
    if bytes.get(..4) != Some(b"PK\x03\x04") && bytes.get(..4) != Some(b"PK\x05\x06") {
        return Err(ParseError::InvalidContent);
    }
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| ParseError::InvalidContent)
}

fn checked_name(raw: &[u8], directory: bool) -> Result<String, ParseError> {
    let name = std::str::from_utf8(raw).map_err(|_| ParseError::InvalidContent)?;
    if name.is_empty() || name.len() > 4096
        || name.chars().any(|c| c.is_control() || matches!(c, '\\' | ':')) {
        return Err(ParseError::InvalidContent);
    }
    let body = if directory { name.strip_suffix('/').unwrap_or(name) } else { name };
    let mut count = 0;
    for component in body.split('/') {
        if matches!(component, "" | "." | "..") { return Err(ParseError::InvalidContent); }
        count += 1;
        if count > MAX_DEPTH { return Err(ParseError::LimitExceeded); }
    }
    Ok(name.to_owned())
}

fn nested_signature(bytes: &[u8]) -> bool {
    bytes.get(..4) == Some(b"PK\x03\x04") || bytes.get(..4) == Some(b"PK\x05\x06")
        || bytes.get(..2) == Some(&[0x1f, 0x8b])
        || bytes.get(..6) == Some(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c])
        || bytes.get(..4) == Some(b"Rar!")
        || bytes.get(..3) == Some(b"BZh")
        || bytes.get(..6) == Some(&[0xfd, b'7', b'z', b'X', b'Z', 0])
}

/// Read every entry to EOF so the zip reader validates its CRC. Nothing is
/// extracted to the filesystem; filenames remain untrusted inventory data.
pub(crate) fn inspect(bytes: &[u8], reject_nested: bool) -> Result<Summary, ParseError> {
    let mut zip = open(bytes)?;
    if zip.len() > MAX_ENTRIES { return Err(ParseError::LimitExceeded); }
    let entries = zip.len() as u32;
    let mut names = BTreeSet::new();
    let mut expanded_bytes = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|_| ParseError::InvalidContent)?;
        let name = checked_name(entry.name_raw(), entry.is_dir())?;
        if !names.insert(name.clone()) { return Err(ParseError::InvalidContent); }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if !matches!(kind, 0 | 0o100000 | 0o040000)
                || (kind == 0o040000 && !entry.is_dir()) {
                return Err(ParseError::InvalidContent);
            }
        }
        if entry.size() > MAX_EXPANDED_BYTES.saturating_sub(expanded_bytes)
            || entry.size() > entry.compressed_size().max(1).saturating_mul(MAX_RATIO) {
            return Err(ParseError::LimitExceeded);
        }
        if reject_nested {
            let extension = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
            if matches!(extension.as_deref(), Some("zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz")) {
                return Err(ParseError::InvalidContent);
            }
        }
        let mut actual = 0u64;
        let mut prefix = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = entry.read(&mut buffer).map_err(|_| ParseError::InvalidContent)?;
            if read == 0 { break; }
            if prefix.len() < 512 {
                let take = read.min(512 - prefix.len());
                prefix.extend_from_slice(&buffer[..take]);
            }
            actual = actual.checked_add(read as u64).ok_or(ParseError::LimitExceeded)?;
            if actual > entry.size() || actual > MAX_EXPANDED_BYTES.saturating_sub(expanded_bytes) {
                return Err(ParseError::LimitExceeded);
            }
        }
        if actual != entry.size() || (entry.is_dir() && actual != 0) {
            return Err(ParseError::InvalidContent);
        }
        if reject_nested && (nested_signature(&prefix) || prefix.get(257..262) == Some(b"ustar")) {
            return Err(ParseError::InvalidContent);
        }
        expanded_bytes += actual;
    }
    Ok(Summary { entries, expanded_bytes })
}

pub(crate) fn part(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str)
    -> Result<Option<String>, ParseError>
{
    let entry = match zip.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err(ParseError::InvalidContent),
    };
    if entry.size() > MAX_XML_BYTES { return Err(ParseError::LimitExceeded); }
    let mut bytes = Vec::new();
    entry.take(MAX_XML_BYTES + 1).read_to_end(&mut bytes).map_err(|_| ParseError::InvalidContent)?;
    if bytes.len() as u64 > MAX_XML_BYTES { return Err(ParseError::LimitExceeded); }
    String::from_utf8(bytes).map(Some).map_err(|_| ParseError::InvalidContent)
}
