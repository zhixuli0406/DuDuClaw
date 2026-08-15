//! `DocumentLimits` — resource ceilings for inbound office / compressed
//! documents (WP-4G, OfficeCLI research §2.5 `Core/DocumentLimits.cs`).
//!
//! ## Why this exists
//!
//! `.docx` / `.xlsx` / `.pptx` are zip containers full of XML. The gateway is a
//! **long-lived single-binary process**: an OOM or a stack overflow taken while
//! parsing one user's attachment kills every agent, channel and scheduler at
//! once. Rust cannot catch a stack overflow at all — it aborts the process.
//!
//! DuDuClaw does not parse OOXML in-process (see the module inventory in the
//! WP-4G report): the actual parsers are **out of process** — LibreOffice
//! (`soffice --convert-to pdf`, the dashboard file preview) and the bundled
//! Python document skills driven by the `office_script` MCP tool. That does not
//! make the problem go away; it moves it. A zip bomb still fills the host's
//! RAM and disk, and a 5000-deep XML tree still crashes a recursive-descent
//! parser — the only difference is which process dies.
//!
//! So this module is a **pre-parse gate**: a cheap, bounded, zero-recursion
//! inspection run *before* the container is handed to any parser. Each ceiling
//! blocks one specific way to die:
//!
//! | Ceiling | Default | Blocks |
//! |---|---|---|
//! | [`DocumentLimits::max_uncompressed_bytes`] | 256 MiB | classic zip bomb — a few KB that inflate to gigabytes |
//! | [`DocumentLimits::max_entries`] | 4096 | a million micro-entries exhausting the parser one file handle at a time (a real OOXML has tens of parts, not thousands) |
//! | [`DocumentLimits::max_compression_ratio`] | 100 | a *single* absurdly-compressible entry; normal OOXML XML sits well under 100× |
//! | [`DocumentLimits::max_xml_depth`] | 128 | deeply-nested XML driving a recursive-descent parser into stack exhaustion |
//! | [`MAX_CONTAINER_DEPTH`] | 4 | zip-inside-zip-inside-zip amplification through embedded objects |
//!
//! There is no `regex_timeout` here, unlike the OfficeCLI original: no
//! DuDuClaw parse path accepts a user-supplied regular expression, so a
//! timeout knob would be dead config. Adding one when nothing can use it would
//! be a false claim of coverage.
//!
//! ## Discipline
//!
//! - **Fail-closed.** Over any ceiling ⇒ the document is refused, with a
//!   zh-TW message the end user can act on. Never truncated, never partially
//!   parsed, never silently downgraded.
//! - **Config cannot open a hole.** Limits come from `config.toml [limits]`;
//!   a missing file, a missing section, a malformed section, or a `0` value
//!   all resolve to the defaults. There is no "unlimited" setting.
//! - **The guard itself is not bombable.** Container nesting is walked with an
//!   explicit work stack and XML depth is counted by a flat byte scanner —
//!   neither recurses, so the checker can never be the thing that overflows.
//! - **No internal paths in user-facing text.** Callers pass the bare display
//!   filename; entry names quoted back are the *document's own* content,
//!   truncated CJK-safely.

use std::io::{Cursor, Read, Seek};
use std::path::Path;

use serde::Deserialize;

// ── Defaults ────────────────────────────────────────────────────

/// Total declared uncompressed bytes across every entry (all nesting levels).
pub const DEFAULT_MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Total entry count across every nesting level.
pub const DEFAULT_MAX_ENTRIES: u32 = 4096;
/// Per-entry `uncompressed / compressed` ceiling.
pub const DEFAULT_MAX_COMPRESSION_RATIO: u32 = 100;
/// XML element nesting ceiling.
pub const DEFAULT_MAX_XML_DEPTH: u32 = 128;
/// How much of a single XML part is depth-scanned.
pub const DEFAULT_XML_SCAN_PART_BYTES: u64 = 8 * 1024 * 1024;
/// How much XML is depth-scanned across the whole document.
pub const DEFAULT_XML_SCAN_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Container nesting ceiling (zip inside zip inside …). Not configurable: a
/// document whose embedded objects nest five archives deep is an attack, not a
/// document, and no legitimate authoring tool produces one.
pub const MAX_CONTAINER_DEPTH: u32 = 4;
/// Declared size ceiling for an entry we will pull into memory to inspect as a
/// nested container. A larger embedded archive is refused rather than buffered.
pub const MAX_NESTED_CONTAINER_BYTES: u64 = 32 * 1024 * 1024;
/// Entries compressing to fewer than this many bytes are exempt from the ratio
/// check — a 40-byte `.rels` stub legitimately compresses to a handful of bytes
/// and would otherwise look like a 20× "bomb". The total-uncompressed ceiling
/// is what actually bounds a swarm of such entries, so the exemption is safe.
pub const RATIO_MIN_COMPRESSED_BYTES: u64 = 1024;
/// Longest entry name echoed back in an error message.
const MAX_ENTRY_NAME_BYTES: usize = 80;

// ── Config ──────────────────────────────────────────────────────

/// Ceilings applied to one inbound document. Read from `config.toml [limits]`.
///
/// `#[serde(default)]` at the container level means an absent or partial
/// section is always valid, and [`DocumentLimits::sanitized`] additionally
/// replaces any zero with its default — a config file can tune this gate but
/// can never disable it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DocumentLimits {
    /// Sum of every entry's declared uncompressed size, across all nesting
    /// levels. The zip-bomb ceiling.
    pub max_uncompressed_bytes: u64,
    /// Total entries across all nesting levels.
    pub max_entries: u32,
    /// Per-entry `uncompressed / compressed`, for entries compressing to at
    /// least [`RATIO_MIN_COMPRESSED_BYTES`].
    pub max_compression_ratio: u32,
    /// XML element nesting ceiling — the guard for downstream recursive-descent
    /// parsers (LibreOffice, python-docx/openpyxl/python-pptx).
    pub max_xml_depth: u32,
    /// Bytes of one XML part that get depth-scanned (see the "known limits"
    /// note on [`inspect_container`]).
    pub xml_scan_part_bytes: u64,
    /// Bytes of XML depth-scanned across the whole document.
    pub xml_scan_total_bytes: u64,
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            max_uncompressed_bytes: DEFAULT_MAX_UNCOMPRESSED_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_compression_ratio: DEFAULT_MAX_COMPRESSION_RATIO,
            max_xml_depth: DEFAULT_MAX_XML_DEPTH,
            xml_scan_part_bytes: DEFAULT_XML_SCAN_PART_BYTES,
            xml_scan_total_bytes: DEFAULT_XML_SCAN_TOTAL_BYTES,
        }
    }
}

impl DocumentLimits {
    /// Load `[limits]` from `<home>/config.toml`.
    ///
    /// The section is parsed in isolation from a generic `toml::Table`, so an
    /// unrelated broken section elsewhere in the file can never affect it.
    /// Missing file / missing section / malformed section ⇒ [`Default`]. The
    /// absence of configuration is never a reason to let a document through.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::from_toml_str(&content)
    }

    /// [`DocumentLimits::from_home`] over an already-read `config.toml` body.
    pub fn from_toml_str(content: &str) -> Self {
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("limits") {
            Some(section) => section
                .clone()
                .try_into::<DocumentLimits>()
                .unwrap_or_default()
                .sanitized(),
            None => Self::default(),
        }
    }

    /// Replace every zero with its default. `0` in a limits table reads as
    /// "unlimited" to a careless operator; here it means "use the default",
    /// because a gate that can be switched off from a config file is not a gate.
    fn sanitized(mut self) -> Self {
        let d = Self::default();
        if self.max_uncompressed_bytes == 0 {
            self.max_uncompressed_bytes = d.max_uncompressed_bytes;
        }
        if self.max_entries == 0 {
            self.max_entries = d.max_entries;
        }
        if self.max_compression_ratio == 0 {
            self.max_compression_ratio = d.max_compression_ratio;
        }
        if self.max_xml_depth == 0 {
            self.max_xml_depth = d.max_xml_depth;
        }
        if self.xml_scan_part_bytes == 0 {
            self.xml_scan_part_bytes = d.xml_scan_part_bytes;
        }
        if self.xml_scan_total_bytes == 0 {
            self.xml_scan_total_bytes = d.xml_scan_total_bytes;
        }
        self
    }
}

// ── Violations ──────────────────────────────────────────────────

/// Why a document was refused. Every variant is a hard DENY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitViolation {
    /// The file could not be opened or read at all.
    Unreadable,
    /// The bytes start like a zip but do not parse as one.
    Malformed,
    TooManyEntries {
        count: u64,
        max: u32,
    },
    UncompressedTooLarge {
        bytes: u64,
        max: u64,
    },
    CompressionRatio {
        entry: String,
        ratio: u64,
        max: u32,
    },
    XmlTooDeep {
        entry: String,
        max: u32,
    },
    ContainerTooDeep {
        max: u32,
    },
    NestedContainerTooLarge {
        entry: String,
        bytes: u64,
        max: u64,
    },
}

impl LimitViolation {
    /// Stable machine tag for logs / metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::Malformed => "malformed",
            Self::TooManyEntries { .. } => "too_many_entries",
            Self::UncompressedTooLarge { .. } => "uncompressed_too_large",
            Self::CompressionRatio { .. } => "compression_ratio",
            Self::XmlTooDeep { .. } => "xml_too_deep",
            Self::ContainerTooDeep { .. } => "container_too_deep",
            Self::NestedContainerTooLarge { .. } => "nested_container_too_large",
        }
    }

    /// zh-TW message for the end user. `display_name` is the bare filename the
    /// caller already shows in the UI — no absolute path, no internal directory
    /// layout, ever reaches this string.
    pub fn user_message(&self, display_name: &str) -> String {
        let name = duduclaw_core::truncate_bytes(display_name, MAX_ENTRY_NAME_BYTES);
        match self {
            Self::Unreadable => format!("無法讀取「{name}」，檔案可能已被移除或損毀。"),
            Self::Malformed => {
                format!("「{name}」不是可解析的 Office 檔（壓縮結構損毀），已拒絕開啟。")
            }
            Self::TooManyEntries { count, max } => format!(
                "「{name}」內含 {count} 個項目，超過安全上限 {max} 個，已拒絕開啟以保護系統。"
            ),
            Self::UncompressedTooLarge { bytes, max } => format!(
                "「{name}」解壓後約需 {} MB，超過安全上限 {} MB，已拒絕開啟以保護系統。",
                mib(*bytes),
                mib(*max)
            ),
            Self::CompressionRatio { entry, ratio, max } => format!(
                "「{name}」中的項目「{}」壓縮比達 {ratio}:1，超過安全上限 {max}:1（疑似壓縮炸彈），已拒絕開啟。",
                duduclaw_core::truncate_bytes(entry, MAX_ENTRY_NAME_BYTES)
            ),
            Self::XmlTooDeep { entry, max } => format!(
                "「{name}」中的內容「{}」巢狀層數超過安全上限 {max} 層，解析可能導致系統崩潰，已拒絕開啟。",
                duduclaw_core::truncate_bytes(entry, MAX_ENTRY_NAME_BYTES)
            ),
            Self::ContainerTooDeep { max } => format!(
                "「{name}」的內嵌壓縮檔層數超過安全上限 {max} 層，已拒絕開啟。"
            ),
            Self::NestedContainerTooLarge { entry, bytes, max } => format!(
                "「{name}」中的內嵌壓縮檔「{}」約 {} MB，超過可檢查上限 {} MB，已拒絕開啟。",
                duduclaw_core::truncate_bytes(entry, MAX_ENTRY_NAME_BYTES),
                mib(*bytes),
                mib(*max)
            ),
        }
    }
}

/// Bytes → whole MiB, rounded up, floor 1 (so a sub-MiB number never prints
/// as "0 MB" in a message about something being too large).
fn mib(bytes: u64) -> u64 {
    bytes.div_ceil(1024 * 1024).max(1)
}

// ── Public entry points ─────────────────────────────────────────

/// True when `bytes` opens with a zip local-file or end-of-central-directory
/// signature. Non-zip document types (legacy `.doc`/`.xls`/`.ppt` OLE, `.csv`,
/// `.pdf`) carry no decompression amplification and are outside this gate.
pub fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == b'P' && bytes[1] == b'K' && matches!(bytes[2..4], [3, 4] | [5, 6])
}

/// Pre-parse gate for a document on disk.
///
/// `Ok(())` means either "this is not a zip container, nothing here to bound"
/// or "every ceiling cleared". `Err` means the caller must refuse to parse —
/// do not fall back, do not truncate, do not hand the bytes to LibreOffice or
/// a Python skill anyway.
pub fn guard_document_path(path: &Path, limits: &DocumentLimits) -> Result<(), LimitViolation> {
    let mut file = std::fs::File::open(path).map_err(|_| LimitViolation::Unreadable)?;
    let mut magic = [0u8; 4];
    match file.read(&mut magic) {
        Ok(n) if looks_like_zip(&magic[..n]) => {}
        Ok(_) => return Ok(()), // not a zip container — out of scope
        Err(_) => return Err(LimitViolation::Unreadable),
    }
    file.rewind().map_err(|_| LimitViolation::Unreadable)?;
    inspect_container(file, limits)
}

/// [`guard_document_path`] over bytes already in memory (used by tests and by
/// callers that hold the buffer).
pub fn guard_document_bytes(bytes: &[u8], limits: &DocumentLimits) -> Result<(), LimitViolation> {
    if !looks_like_zip(bytes) {
        return Ok(());
    }
    inspect_container(Cursor::new(bytes.to_vec()), limits)
}

// ── Scanner ─────────────────────────────────────────────────────

/// Running totals shared across every nesting level of one document.
#[derive(Default)]
struct ScanState {
    entries: u64,
    uncompressed: u64,
    xml_scanned: u64,
}

/// Walk a zip container and every archive nested inside it.
///
/// Container nesting is an explicit work stack, never recursion: the deepest
/// document cannot make *this function* exhaust the stack, which would be a
/// spectacular own goal for a guard whose whole job is preventing exactly that.
///
/// ### Known limits (stated, not papered over)
///
/// - Sizes come from the zip central directory. A **lying header** (declares
///   small, inflates large) is not detected here — nothing short of full
///   decompression could. This gate's job is to stop the declared-size bombs
///   and the structural bombs before a parser is even spawned; the downstream
///   parser's own memory ceiling is the second line for a lying header.
/// - XML depth is scanned over the first [`DocumentLimits::xml_scan_part_bytes`]
///   of each part, up to [`DocumentLimits::xml_scan_total_bytes`] overall. Past
///   that budget the scan stops. It does **not** then reject the document — a
///   legitimately huge spreadsheet must still open — so deep nesting hidden
///   beyond the budget is a residual gap, deliberately traded against false
///   positives on real documents.
pub fn inspect_container<R: Read + Seek>(
    reader: R,
    limits: &DocumentLimits,
) -> Result<(), LimitViolation> {
    let mut state = ScanState::default();
    // (bytes, depth) for archives found nested inside the one being scanned.
    let mut pending: Vec<(Vec<u8>, u32)> = Vec::new();

    let mut nested = scan_one(reader, 0, limits, &mut state)?;
    pending.append(&mut nested);

    while let Some((bytes, depth)) = pending.pop() {
        let mut more = scan_one(Cursor::new(bytes), depth, limits, &mut state)?;
        pending.append(&mut more);
    }
    Ok(())
}

/// Inspect one archive level. Returns the nested archives to enqueue.
fn scan_one<R: Read + Seek>(
    reader: R,
    depth: u32,
    limits: &DocumentLimits,
    state: &mut ScanState,
) -> Result<Vec<(Vec<u8>, u32)>, LimitViolation> {
    if depth >= MAX_CONTAINER_DEPTH {
        return Err(LimitViolation::ContainerTooDeep {
            max: MAX_CONTAINER_DEPTH,
        });
    }
    let mut archive = zip::ZipArchive::new(reader).map_err(|_| LimitViolation::Malformed)?;

    state.entries = state.entries.saturating_add(archive.len() as u64);
    if state.entries > limits.max_entries as u64 {
        return Err(LimitViolation::TooManyEntries {
            count: state.entries,
            max: limits.max_entries,
        });
    }

    let mut nested: Vec<(Vec<u8>, u32)> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| LimitViolation::Malformed)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let size = entry.size();
        let packed = entry.compressed_size();

        state.uncompressed = state.uncompressed.saturating_add(size);
        if state.uncompressed > limits.max_uncompressed_bytes {
            return Err(LimitViolation::UncompressedTooLarge {
                bytes: state.uncompressed,
                max: limits.max_uncompressed_bytes,
            });
        }

        if packed >= RATIO_MIN_COMPRESSED_BYTES {
            let ratio = size / packed;
            if ratio > limits.max_compression_ratio as u64 {
                return Err(LimitViolation::CompressionRatio {
                    entry: name,
                    ratio,
                    max: limits.max_compression_ratio,
                });
            }
        }

        let lower = name.to_ascii_lowercase();
        if is_xml_part(&lower) {
            let budget = limits
                .xml_scan_part_bytes
                .min(limits.xml_scan_total_bytes.saturating_sub(state.xml_scanned));
            if budget > 0 {
                let mut buf = Vec::new();
                // Bounded read: the guard never allocates more than the budget,
                // whatever the header claims.
                if entry.by_ref().take(budget).read_to_end(&mut buf).is_err() {
                    return Err(LimitViolation::Malformed);
                }
                state.xml_scanned = state.xml_scanned.saturating_add(buf.len() as u64);
                if scan_xml_depth(&buf, limits.max_xml_depth).is_err() {
                    return Err(LimitViolation::XmlTooDeep {
                        entry: name,
                        max: limits.max_xml_depth,
                    });
                }
            }
        } else if is_nested_container(&lower) {
            if size > MAX_NESTED_CONTAINER_BYTES {
                return Err(LimitViolation::NestedContainerTooLarge {
                    entry: name,
                    bytes: size,
                    max: MAX_NESTED_CONTAINER_BYTES,
                });
            }
            let mut buf = Vec::new();
            if entry
                .by_ref()
                .take(MAX_NESTED_CONTAINER_BYTES)
                .read_to_end(&mut buf)
                .is_err()
            {
                return Err(LimitViolation::Malformed);
            }
            if looks_like_zip(&buf) {
                nested.push((buf, depth + 1));
            }
        }
    }
    Ok(nested)
}

/// OOXML parts whose XML is worth depth-scanning.
fn is_xml_part(lower_name: &str) -> bool {
    lower_name.ends_with(".xml") || lower_name.ends_with(".rels")
}

/// Entry names that may themselves be zip containers (OOXML embedded objects).
fn is_nested_container(lower_name: &str) -> bool {
    [".zip", ".docx", ".xlsx", ".pptx", ".odt", ".ods", ".odp"]
        .iter()
        .any(|ext| lower_name.ends_with(ext))
}

/// Flat, allocation-free XML nesting-depth scanner.
///
/// Returns the deepest nesting seen, or `Err(depth)` the moment `max` is
/// exceeded. Quoted attribute values are tracked so a `>` inside an attribute
/// cannot desynchronise the count (which would manufacture false positives on
/// legitimate documents); processing instructions, comments/DOCTYPE and CDATA
/// sections are skipped rather than counted.
fn scan_xml_depth(buf: &[u8], max: u32) -> Result<u32, u32> {
    let mut depth: u32 = 0;
    let mut deepest: u32 = 0;
    let mut i: usize = 0;
    let n = buf.len();

    while i < n {
        if buf[i] != b'<' {
            i += 1;
            continue;
        }
        match buf.get(i + 1).copied() {
            Some(b'/') => {
                depth = depth.saturating_sub(1);
                i = scan_tag_end(buf, i + 2).0;
            }
            Some(b'?') => {
                i = scan_tag_end(buf, i + 2).0;
            }
            Some(b'!') => {
                let rest = buf.get(i + 2..).unwrap_or(&[]);
                i = if rest.starts_with(b"[CDATA[") {
                    skip_past_cdata(buf, i + 9)
                } else {
                    scan_tag_end(buf, i + 2).0
                };
            }
            Some(_) => {
                let (end, found, self_closing) = scan_tag_end(buf, i + 1);
                if found {
                    // A self-closing element still occupies a level while the
                    // parser is inside it, so it counts toward the peak and
                    // then unwinds. Deliberately the conservative reading:
                    // this number guards a call stack, not a schema.
                    let level = depth.saturating_add(1);
                    deepest = deepest.max(level);
                    if level > max {
                        return Err(level);
                    }
                    if !self_closing {
                        depth = level;
                    }
                }
                i = end;
            }
            None => break,
        }
    }
    Ok(deepest)
}

/// Scan forward to a tag-closing `>`, honoring quoted attribute values.
/// Returns `(index_just_past_gt, found_gt, byte_before_gt_was_slash)`.
fn scan_tag_end(buf: &[u8], from: usize) -> (usize, bool, bool) {
    let mut i = from.min(buf.len());
    let mut quote: u8 = 0;
    let mut prev: u8 = 0;
    while i < buf.len() {
        let b = buf[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return (i + 1, true, prev == b'/');
        }
        prev = b;
        i += 1;
    }
    (buf.len(), false, false)
}

/// Index just past the terminating `]]>` of a CDATA section.
fn skip_past_cdata(buf: &[u8], from: usize) -> usize {
    let start = from.min(buf.len());
    let mut i = start;
    while i + 2 < buf.len() {
        if &buf[i..i + 3] == b"]]>" {
            return i + 3;
        }
        i += 1;
    }
    buf.len()
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    /// Build a zip in memory. `entries` are `(name, contents)`.
    fn build_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let opts: FileOptions<'_, ()> =
                FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(body).unwrap();
            }
            zw.finish().unwrap();
        }
        cursor.into_inner()
    }

    /// A minimally realistic `.docx`: content types, rels, and a document part.
    fn normal_docx() -> Vec<u8> {
        let body = format!(
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>{}</w:body></w:document>",
            "<w:p><w:r><w:t>報告內容 with a &gt; sign and attr=\"a>b\"</w:t></w:r></w:p>"
                .repeat(200)
        );
        build_zip(&[
            (
                "[Content_Types].xml",
                b"<?xml version=\"1.0\"?><Types/>".to_vec(),
            ),
            (
                "_rels/.rels",
                b"<?xml version=\"1.0\"?><Relationships><Relationship Id=\"r1\"/></Relationships>"
                    .to_vec(),
            ),
            ("word/document.xml", body.into_bytes()),
        ])
    }

    #[test]
    fn normal_docx_passes() {
        let limits = DocumentLimits::default();
        let zip = normal_docx();
        assert_eq!(guard_document_bytes(&zip, &limits), Ok(()));
    }

    #[test]
    fn non_zip_input_is_out_of_scope() {
        let limits = DocumentLimits::default();
        // A PDF / CSV / legacy .doc carries no decompression amplification.
        assert_eq!(guard_document_bytes(b"%PDF-1.4 ...", &limits), Ok(()));
        assert_eq!(guard_document_bytes(b"a,b,c\n1,2,3\n", &limits), Ok(()));
        assert_eq!(guard_document_bytes(b"", &limits), Ok(()));
    }

    #[test]
    fn zip_bomb_high_ratio_entry_is_rejected() {
        // 4 MiB of zeros deflates to a few KB — ratio far above 100:1, while
        // the file on disk stays tiny. This is the classic shape.
        let zip = build_zip(&[("word/bomb.bin", vec![0u8; 4 * 1024 * 1024])]);
        assert!(
            zip.len() < 64 * 1024,
            "test fixture should stay small: {} bytes",
            zip.len()
        );
        let err = guard_document_bytes(&zip, &DocumentLimits::default()).unwrap_err();
        match &err {
            LimitViolation::CompressionRatio { ratio, max, .. } => {
                assert!(ratio > &(*max as u64), "{err:?}");
            }
            other => panic!("expected a compression-ratio rejection, got {other:?}"),
        }
        // The message is user-readable zh-TW, names the file the user knows,
        // and quotes only the document's OWN internal entry name.
        let msg = err.user_message("季報.docx");
        assert!(msg.contains("壓縮炸彈"), "{msg}");
        assert!(msg.contains("季報.docx"), "{msg}");
        assert!(msg.contains("word/bomb.bin"), "{msg}");
    }

    #[test]
    fn total_uncompressed_ceiling_is_enforced() {
        // Each entry sits under the ratio ceiling, but together they blow the
        // declared-total budget — the two guards have to compose.
        let limits = DocumentLimits {
            max_uncompressed_bytes: 512 * 1024,
            ..Default::default()
        };
        // Deliberately incompressible (LCG high bytes): a repeating pattern
        // would trip the ratio ceiling first and this test would pass for the
        // wrong reason.
        let filler: Vec<u8> = {
            let mut v = Vec::with_capacity(200 * 1024);
            let mut s: u32 = 0x1234_5678;
            for _ in 0..200 * 1024 {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                v.push((s >> 24) as u8);
            }
            v
        };
        let zip = build_zip(&[
            ("a.bin", filler.clone()),
            ("b.bin", filler.clone()),
            ("c.bin", filler),
        ]);
        assert!(matches!(
            guard_document_bytes(&zip, &limits),
            Err(LimitViolation::UncompressedTooLarge { .. })
        ));
    }

    #[test]
    fn entry_count_ceiling_is_enforced() {
        let limits = DocumentLimits {
            max_entries: 16,
            ..Default::default()
        };
        let names: Vec<String> = (0..40).map(|i| format!("part{i}.bin")).collect();
        let entries: Vec<(&str, Vec<u8>)> =
            names.iter().map(|n| (n.as_str(), b"x".to_vec())).collect();
        let zip = build_zip(&entries);
        let err = guard_document_bytes(&zip, &limits).unwrap_err();
        assert!(
            matches!(err, LimitViolation::TooManyEntries { count: 40, max: 16 }),
            "{err:?}"
        );
        assert!(err.user_message("big.xlsx").contains("超過安全上限"));
    }

    #[test]
    fn deeply_nested_xml_is_rejected() {
        let depth = 500usize;
        let mut xml = String::from("<?xml version=\"1.0\"?>");
        for _ in 0..depth {
            xml.push_str("<a>");
        }
        xml.push_str("leaf");
        for _ in 0..depth {
            xml.push_str("</a>");
        }
        let zip = build_zip(&[("word/document.xml", xml.into_bytes())]);
        let err = guard_document_bytes(&zip, &DocumentLimits::default()).unwrap_err();
        assert!(matches!(err, LimitViolation::XmlTooDeep { max: 128, .. }), "{err:?}");
        assert!(err.user_message("deep.docx").contains("巢狀層數"));
    }

    #[test]
    fn xml_depth_counts_are_not_desynchronised_by_markup_noise() {
        // Self-closing tags, PIs, comments, CDATA and a `>` inside a quoted
        // attribute must all leave the depth count intact.
        let xml = r#"<?xml version="1.0"?><!-- a > comment --><root>
            <self attr="a>b"/><![CDATA[ <fake><fake><fake> ]]>
            <child><leaf/></child></root>"#;
        assert_eq!(scan_xml_depth(xml.as_bytes(), 128), Ok(3));
    }

    #[test]
    fn scan_xml_depth_never_recurses_on_pathological_input() {
        // 200k opening tags: the scanner is a flat loop, so this must return a
        // violation rather than blow the stack.
        let xml = "<a>".repeat(200_000);
        assert!(scan_xml_depth(xml.as_bytes(), 128).is_err());
    }

    #[test]
    fn nested_container_depth_is_capped() {
        // zip → zip → zip → zip → zip exceeds MAX_CONTAINER_DEPTH.
        let mut inner = build_zip(&[("leaf.xml", b"<a/>".to_vec())]);
        for _ in 0..MAX_CONTAINER_DEPTH + 1 {
            inner = build_zip(&[("embed.zip", inner)]);
        }
        assert!(matches!(
            guard_document_bytes(&inner, &DocumentLimits::default()),
            Err(LimitViolation::ContainerTooDeep { .. })
        ));
    }

    #[test]
    fn one_level_embedded_object_still_passes() {
        // A docx with a legitimately embedded xlsx object is normal.
        let embedded = build_zip(&[("xl/workbook.xml", b"<workbook/>".to_vec())]);
        let outer = build_zip(&[
            ("word/document.xml", b"<w:document/>".to_vec()),
            ("word/embeddings/book.xlsx", embedded),
        ]);
        assert_eq!(
            guard_document_bytes(&outer, &DocumentLimits::default()),
            Ok(())
        );
    }

    #[test]
    fn nested_bomb_is_caught_through_the_outer_container() {
        let bomb = build_zip(&[("bomb.bin", vec![0u8; 4 * 1024 * 1024])]);
        let outer = build_zip(&[("word/embeddings/inner.zip", bomb)]);
        assert!(matches!(
            guard_document_bytes(&outer, &DocumentLimits::default()),
            Err(LimitViolation::CompressionRatio { .. })
        ));
    }

    #[test]
    fn malformed_zip_is_refused_not_passed_through() {
        // Starts with the zip signature but is not a zip — fail-closed.
        let mut junk = b"PK\x03\x04".to_vec();
        junk.extend_from_slice(&[0xAB; 128]);
        assert_eq!(
            guard_document_bytes(&junk, &DocumentLimits::default()),
            Err(LimitViolation::Malformed)
        );
    }

    // ── config ──────────────────────────────────────────────────

    #[test]
    fn missing_config_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        // No config.toml at all.
        assert_eq!(DocumentLimits::from_home(tmp.path()), DocumentLimits::default());
        // A config.toml with no [limits] section.
        std::fs::write(tmp.path().join("config.toml"), "[general]\nlog_level = \"info\"\n")
            .unwrap();
        assert_eq!(DocumentLimits::from_home(tmp.path()), DocumentLimits::default());
        // And the defaults are the documented numbers.
        let d = DocumentLimits::default();
        assert_eq!(d.max_uncompressed_bytes, 256 * 1024 * 1024);
        assert_eq!(d.max_entries, 4096);
        assert_eq!(d.max_compression_ratio, 100);
        assert_eq!(d.max_xml_depth, 128);
    }

    #[test]
    fn malformed_config_does_not_open_a_hole() {
        // Unparseable file, wrong types, and an unrelated broken section all
        // resolve to defaults rather than to "no limit".
        assert_eq!(DocumentLimits::from_toml_str("this is not toml ["), DocumentLimits::default());
        assert_eq!(
            DocumentLimits::from_toml_str("[limits]\nmax_entries = \"lots\"\n"),
            DocumentLimits::default()
        );
        let d = DocumentLimits::from_toml_str("[limits]\nmax_entries = 32\n");
        assert_eq!(d.max_entries, 32);
        // Untouched keys keep their defaults.
        assert_eq!(d.max_compression_ratio, DEFAULT_MAX_COMPRESSION_RATIO);
    }

    #[test]
    fn zero_in_config_means_default_not_unlimited() {
        let d = DocumentLimits::from_toml_str(
            "[limits]\nmax_entries = 0\nmax_uncompressed_bytes = 0\nmax_compression_ratio = 0\nmax_xml_depth = 0\n",
        );
        assert_eq!(d, DocumentLimits::default());
        // And the gate still bites with that config.
        let zip = build_zip(&[("word/bomb.bin", vec![0u8; 4 * 1024 * 1024])]);
        assert!(guard_document_bytes(&zip, &d).is_err());
    }

    #[test]
    fn guard_document_path_reads_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("ok.docx");
        std::fs::write(&good, normal_docx()).unwrap();
        assert_eq!(
            guard_document_path(&good, &DocumentLimits::default()),
            Ok(())
        );

        let bad = tmp.path().join("bomb.docx");
        std::fs::write(&bad, build_zip(&[("b.bin", vec![0u8; 4 * 1024 * 1024])])).unwrap();
        let err = guard_document_path(&bad, &DocumentLimits::default()).unwrap_err();
        // The user-facing text must never carry the host filesystem path the
        // guard just inspected — only the display name the caller supplies.
        let msg = err.user_message("bomb.docx");
        let host_path = tmp.path().to_string_lossy().to_string();
        assert!(!msg.contains(&host_path), "message leaked a host path: {msg}");

        // Missing file ⇒ Unreadable, never Ok.
        assert_eq!(
            guard_document_path(&tmp.path().join("nope.docx"), &DocumentLimits::default()),
            Err(LimitViolation::Unreadable)
        );
    }
}
