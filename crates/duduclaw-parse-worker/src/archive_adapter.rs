//! ZIP expansion stays in the disposable worker. The caller publishes only
//! bounded names and byte ranges from a verified, sealed output artifact.
use std::io::Read;
use duduclaw_isolation::{bounded::List, worker::*};

pub const MAX_PACK_BYTES: u64 = 50 * 1024 * 1024;

pub struct ExpandedPack {
    pub entries: Vec<ArchiveEntry>,
    pub bytes: Vec<u8>,
    pub junk_skipped: u32,
    pub legacy_encoding: Option<String>,
}

pub fn extract_pack(reader: &mut impl Read) -> Result<ExpandedPack, ParseError> {
    let mut request = crate::client::request(ParseOperation::ExtractPackArchive, ContentFormat::Archive, 0);
    request.options = Some(ParseOptions::PackArchive);
    request.outputs = List::new(vec![OutputFd { slot: 1, max_bytes: MAX_PACK_BYTES.try_into().unwrap() }]).unwrap();
    let (response, mut outputs) = crate::client::invoke_output_reader(&request, reader)?;
    let ParseResult::Complete { items } = response.result else { return Err(ParseError::InvalidContent); };
    let [ParsedItem::ArchiveManifest { input_slot: 0, entries, junk_skipped, legacy_encoding },
        ParsedItem::OutputArtifact { output_slot: 1, format: ContentFormat::Archive, size_bytes, .. }] = items.as_slice()
        else { return Err(ParseError::InvalidContent); };
    if outputs.len() != 1 || *size_bytes > MAX_PACK_BYTES { return Err(ParseError::InvalidContent); }
    let bytes = outputs.remove(0);
    if bytes.len() as u64 != *size_bytes { return Err(ParseError::InvalidContent); }
    let mut offset = 0u64;
    for entry in entries.as_slice() {
        if entry.offset != offset || (entry.directory && entry.length != 0) {
            return Err(ParseError::InvalidContent);
        }
        offset = offset.checked_add(entry.length).ok_or(ParseError::LimitExceeded)?;
        if offset > *size_bytes { return Err(ParseError::InvalidContent); }
    }
    if offset != *size_bytes { return Err(ParseError::InvalidContent); }
    Ok(ExpandedPack { entries: entries.as_slice().to_vec(), bytes,
        junk_skipped: *junk_skipped,
        legacy_encoding: legacy_encoding.as_ref().map(|value| value.as_str().to_owned()) })
}

#[cfg(feature = "engine")]
#[allow(dead_code)]
#[path = "../../duduclaw-cli/src/expert/zip_names.rs"]
mod zip_names;

#[cfg(feature = "engine")]
mod engine {
    use super::*;
    use std::fs::File;
    use std::io::{Cursor, Seek, SeekFrom, Write};
    use duduclaw_isolation::{bounded::Text, ids::Sha256Digest};
    use sha2::{Digest, Sha256};

    pub fn parse(request: &ParseRequest, inputs: &mut [File], outputs: &mut [File])
        -> Option<Result<Vec<ParsedItem>, ParseError>>
    {
        if request.operation != ParseOperation::ExtractPackArchive { return None; }
        Some((|| {
            request.validate().map_err(|_| ParseError::InvalidContent)?;
            if inputs.len() != 1 || outputs.len() != 1
                || request.inputs.as_slice()[0].format != ContentFormat::Archive {
                return Err(ParseError::InvalidContent);
            }
            let input = &mut inputs[0];
            input.seek(SeekFrom::Start(0)).map_err(|_| ParseError::InvalidContent)?;
            let mut bytes = Vec::new();
            input.take(MAX_PACK_BYTES + 1).read_to_end(&mut bytes).map_err(|_| ParseError::InvalidContent)?;
            if bytes.len() as u64 > MAX_PACK_BYTES { return Err(ParseError::LimitExceeded); }
            if bytes.len() as u64 != request.inputs.as_slice()[0].size_bytes { return Err(ParseError::InvalidContent); }
            let (entries, expanded, junk, encoding) = expand(&bytes, request.outputs.as_slice()[0].max_bytes.get())?;
            let output = &mut outputs[0];
            output.seek(SeekFrom::Start(0)).map_err(|_| ParseError::InvalidContent)?;
            output.write_all(&expanded).map_err(|_| ParseError::LimitExceeded)?;
            output.flush().map_err(|_| ParseError::InvalidContent)?;
            Ok(vec![ParsedItem::ArchiveManifest { input_slot: 0, entries: List::new(entries).map_err(|_| ParseError::LimitExceeded)?,
                junk_skipped: junk, legacy_encoding: encoding.map(Text::new).transpose().map_err(|_| ParseError::LimitExceeded)? },
                ParsedItem::OutputArtifact { output_slot: 1, format: ContentFormat::Archive,
                    size_bytes: expanded.len() as u64, sha256: Sha256Digest(Sha256::digest(&expanded).into()) }])
        })())
    }

    fn expand(bytes: &[u8], budget: u64) -> Result<(Vec<ArchiveEntry>, Vec<u8>, u32, Option<String>), ParseError> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| ParseError::InvalidContent)?;
        if zip.len() > 1000 { return Err(ParseError::LimitExceeded); }
        let mut kept = Vec::new();
        let mut names = Vec::new();
        let mut junk = 0;
        for index in 0..zip.len() {
            let entry = zip.by_index(index).map_err(|_| ParseError::InvalidContent)?;
            if entry.enclosed_name().is_none() { return Err(ParseError::InvalidContent); }
            if zip_names::is_junk_raw(entry.name_raw()) { junk += 1; continue; }
            if entry.is_symlink() || entry.unix_mode().is_some_and(|mode| {
                !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000)
                    || ((mode & 0o170000) == 0o040000 && !entry.is_dir())
            }) { return Err(ParseError::InvalidContent); }
            kept.push(index);
            names.push(zip_names::RawName { bytes: entry.name_raw().to_vec(), utf8_flag: entry.flags().as_u16() & 0x0800 != 0 });
        }
        let decoded = zip_names::decode_names(&names).map_err(|_| ParseError::InvalidContent)?;
        let encoding = decoded.legacy.map(|value| match value { zip_names::LegacyEncoding::Gbk => "gbk", zip_names::LegacyEncoding::Big5 => "big5" }.into());
        let mut seen = std::collections::BTreeSet::new();
        let mut output = Vec::new();
        let mut entries = Vec::new();
        let mut name_bytes = 0usize;
        let budget = budget.min(MAX_PACK_BYTES);
        for (index, name) in kept.into_iter().zip(decoded.names) {
            let mut entry = zip.by_index(index).map_err(|_| ParseError::InvalidContent)?;
            let name = name.replace('\\', "/");
            let name = name.trim_end_matches('/');
            if name.is_empty() || name.chars().any(|c| c.is_control() || c == ':')
                || name.split('/').any(|part| matches!(part, "" | "." | "..")) {
                return Err(ParseError::InvalidContent);
            }
            if name.split('/').count() > 32 || !seen.insert(name.to_lowercase()) { return Err(ParseError::InvalidContent); }
            name_bytes = name_bytes.checked_add(name.len()).ok_or(ParseError::LimitExceeded)?;
            if name_bytes > MAX_RESULT_TEXT_BYTES { return Err(ParseError::LimitExceeded); }
            let remaining = budget.saturating_sub(output.len() as u64);
            if entry.size() > remaining || (entry.compressed_size() >= 1024
                && entry.size() / entry.compressed_size() > 100) { return Err(ParseError::LimitExceeded); }
            let offset = output.len() as u64;
            let directory = entry.is_dir();
            if directory && entry.size() != 0 { return Err(ParseError::InvalidContent); }
            let copied = entry.by_ref().take(remaining + 1).read_to_end(&mut output).map_err(|_| ParseError::InvalidContent)?;
            if copied as u64 > remaining { return Err(ParseError::LimitExceeded); }
            if copied as u64 != entry.size() { return Err(ParseError::InvalidContent); }
            entries.push(ArchiveEntry { name: Text::new(name.to_owned()).map_err(|_| ParseError::LimitExceeded)?,
                directory, offset, length: copied as u64 });
        }
        Ok((entries, output, junk, encoding))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (name, bytes) in entries {
                zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap().into_inner()
        }
        #[test]
        fn pack_expansion_returns_contiguous_bytes_and_skips_junk() {
            let bytes = archive(&[("a.txt", b"one"), ("dir/b.txt", b"two"), (".DS_Store", b"junk")]);
            let (entries, data, junk, encoding) = expand(&bytes, 6).unwrap();
            assert_eq!(data, b"onetwo");
            assert_eq!(entries[0].offset, 0);
            assert_eq!(entries[1].offset, 3);
            assert_eq!(junk, 1);
            assert_eq!(encoding, None);
        }
        #[test]
        fn pack_expansion_refuses_budget_and_case_collision() {
            assert!(expand(&archive(&[("a", b"123456")]), 5).is_err());
            assert!(expand(&archive(&[("A", b"1"), ("a", b"2")]), 10).is_err());
        }
    }
}

#[cfg(feature = "engine")]
pub use engine::parse;
