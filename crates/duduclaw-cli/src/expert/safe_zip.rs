//! Fail-closed zip handling for expert packs.
//!
//! - **Extraction** ([`extract_to`]) rejects zip-slip (path traversal /
//!   absolute paths / symlink escape) by canonicalising every entry against a
//!   fixed root fence, caps the total uncompressed size at [`MAX_UNPACK_BYTES`]
//!   (zip-bomb guard), and never follows symlinks.
//! - **Packing** ([`pack_dir`]) deflates a directory tree into a `.zip`.
//!
//! No external `walkdir`/`tempfile` deps — plain `std::fs` recursion.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
// WP-4G: one source of truth for the entry-count and compression-ratio
// ceilings, shared with the inbound-document gate.
use duduclaw_gateway::document_limits as doclimits;

/// Hard ceiling on total uncompressed bytes and on the archive file itself.
pub const MAX_UNPACK_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

/// What an extraction left out or had to guess — surfaced in the import
/// report so nothing is dropped silently.
#[derive(Debug, Clone, Default)]
pub struct ExtractSummary {
    /// OS clutter skipped (`__MACOSX/`, `._*`, `.DS_Store`, `Thumbs.db`, …).
    pub junk_skipped: usize,
    /// Symbolic-link entries skipped (never extracted).
    pub links_skipped: Vec<String>,
    /// Legacy filename encoding the archive needed, if any.
    pub legacy_encoding: Option<super::zip_names::LegacyEncoding>,
}

/// Extract `zip_path` into `dest_root` (which is created). Every entry is
/// contained under `dest_root`; a traversing entry aborts the whole extraction
/// (fail-closed — a partially-trusted tree is never left on disk on error, the
/// caller extracts into a throwaway temp dir).
///
/// OS clutter is skipped and entry names are decoded by
/// [`super::zip_names::decode_names`] (UTF-8 flag → 0x7075 → UTF-8 → GBK /
/// Big5); an archive whose names cannot be decoded is refused rather than
/// written as mojibake.
pub fn extract_to(zip_path: &Path, dest_root: &Path) -> Result<ExtractSummary> {
    let file_len = std::fs::metadata(zip_path)
        .map_err(|e| io_err(format!("讀取 {} 失敗: {e}", zip_path.display())))?
        .len();
    if file_len > MAX_UNPACK_BYTES {
        return Err(cfg_err(format!(
            "壓縮檔過大（{file_len} bytes > 上限 {MAX_UNPACK_BYTES}）"
        )));
    }

    let file = std::fs::File::open(zip_path)
        .map_err(|e| io_err(format!("開啟 {} 失敗: {e}", zip_path.display())))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| cfg_err(format!("非合法 zip：{e}")))?;

    // WP-4G ceiling 1 — entry count. A million micro-entries exhausts inodes
    // and file handles long before the byte budget notices anything. Counted
    // before junk filtering: clutter still costs a header read each.
    if archive.len() as u64 > doclimits::DEFAULT_MAX_ENTRIES as u64 {
        return Err(cfg_err(format!(
            "壓縮檔條目數 {} 超過上限 {}（疑似耗盡型攻擊）",
            archive.len(),
            doclimits::DEFAULT_MAX_ENTRIES
        )));
    }

    let plan = plan_entries(&mut archive)?;

    std::fs::create_dir_all(dest_root)
        .map_err(|e| io_err(format!("建立 {} 失敗: {e}", dest_root.display())))?;
    // Canonical fence — every written path must stay under this.
    let root_canon = dest_root
        .canonicalize()
        .map_err(|e| io_err(format!("canonicalize {} 失敗: {e}", dest_root.display())))?;

    // `total` tracks the sizes the archive *declares*; `written` tracks what
    // actually lands on disk. Both are needed: a declared-size bomb is caught
    // early and cheaply by `total`, and a lying header (declares tiny, inflates
    // huge) is caught only by `written` — the gap WP-4G closed here.
    let mut total: u64 = 0;
    let mut written: u64 = 0;
    for (i, rel) in &plan.entries {
        let mut entry = archive
            .by_index(*i)
            .map_err(|e| cfg_err(format!("zip 條目 {i} 讀取失敗: {e}")))?;

        let out_path = dest_root.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| io_err(format!("建立目錄失敗: {e}")))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(format!("建立目錄失敗: {e}")))?;
        }

        // Post-join canonical fence: the parent must resolve under root_canon.
        // (Guards against a symlinked ancestor smuggled by an earlier entry.)
        if let Some(parent) = out_path.parent()
            && let Ok(pc) = parent.canonicalize()
            && !pc.starts_with(&root_canon)
        {
            return Err(cfg_err(format!(
                "拒絕逃逸解包圍欄的條目：{}",
                rel.display()
            )));
        }

        total = total.saturating_add(entry.size());
        if total > MAX_UNPACK_BYTES {
            return Err(cfg_err(format!(
                "解包總量超過上限 {MAX_UNPACK_BYTES} bytes（疑似 zip bomb）"
            )));
        }

        // WP-4G ceiling 2 — per-entry compression ratio. Entries compressing to
        // under the floor are exempt (a tiny stub legitimately compresses hard);
        // the total budget is what bounds a swarm of those.
        let packed = entry.compressed_size();
        if packed >= doclimits::RATIO_MIN_COMPRESSED_BYTES {
            let ratio = entry.size() / packed;
            if ratio > doclimits::DEFAULT_MAX_COMPRESSION_RATIO as u64 {
                return Err(cfg_err(format!(
                    "條目 {} 壓縮比 {ratio}:1 超過上限 {}:1（疑似 zip bomb）",
                    rel.display(),
                    doclimits::DEFAULT_MAX_COMPRESSION_RATIO
                )));
            }
        }

        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| io_err(format!("寫入 {} 失敗: {e}", out_path.display())))?;
        // WP-4G ceiling 3 — size-limited copy against the *remaining* budget,
        // not a fresh `MAX_UNPACK_BYTES` per entry. The old per-entry `take`
        // let N entries with lying headers write N × 50 MB while `total` (which
        // only ever saw the declared sizes) stayed at zero. Take one byte past
        // the budget so an over-budget entry is *detected*, never silently
        // truncated into a corrupt file.
        let remaining = MAX_UNPACK_BYTES.saturating_sub(written);
        let mut limited = entry.by_ref().take(remaining.saturating_add(1));
        let n = std::io::copy(&mut limited, &mut out)
            .map_err(|e| io_err(format!("解壓 {} 失敗: {e}", rel.display())))?;
        written = written.saturating_add(n);
        if written > MAX_UNPACK_BYTES {
            return Err(cfg_err(format!(
                "解包實際輸出超過上限 {MAX_UNPACK_BYTES} bytes（zip 標頭宣告不實，疑似 zip bomb）"
            )));
        }
    }

    Ok(plan.summary)
}

/// The entries to extract (index + decoded, validated relative path) and what
/// was left out. Reads headers only.
struct ExtractPlan {
    entries: Vec<(usize, PathBuf)>,
    summary: ExtractSummary,
}

fn plan_entries(archive: &mut zip::ZipArchive<std::fs::File>) -> Result<ExtractPlan> {
    use super::zip_names::{RawName, decode_names, is_junk_raw};

    let mut summary = ExtractSummary::default();
    let mut kept: Vec<usize> = Vec::new();
    let mut raw: Vec<RawName> = Vec::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| cfg_err(format!("zip 條目 {i} 讀取失敗: {e}")))?;
        // `enclosed_name` (CP437/UTF-8 per the crate) already rejects `..`
        // and absolute paths; those are ASCII, so the crate's own decoding is
        // good enough for this first fence. The decoded name is fenced again
        // below.
        if entry.enclosed_name().is_none() {
            return Err(cfg_err(format!(
                "拒絕不安全的 zip 條目（zip-slip）：{}",
                String::from_utf8_lossy(entry.name_raw())
            )));
        }
        let bytes = entry.name_raw().to_vec();
        if is_junk_raw(&bytes) {
            summary.junk_skipped += 1;
            continue;
        }
        if entry.is_symlink() {
            summary
                .links_skipped
                .push(String::from_utf8_lossy(&bytes).into_owned());
            continue;
        }
        let utf8_flag = entry.flags().as_u16() & 0x0800 != 0;
        kept.push(i);
        raw.push(RawName { bytes, utf8_flag });
    }

    let decoded = decode_names(&raw).map_err(cfg_err)?;
    summary.legacy_encoding = decoded.legacy;

    let mut entries = Vec::with_capacity(kept.len());
    // Two entries that land on the same path on a case-insensitive file
    // system (macOS, Windows) would silently overwrite each other; refuse
    // the archive instead of guessing which one wins.
    let mut seen = std::collections::BTreeSet::new();
    for (i, name) in kept.into_iter().zip(decoded.names) {
        let rel = decoded_rel_path(&name)?;
        let key = rel.to_string_lossy().to_lowercase();
        if !seen.insert(key) {
            return Err(cfg_err(format!(
                "壓縮檔內有兩個項目的路徑相同（只差大小寫）：{}，無法判斷要用哪一個，已停止匯入",
                rel.display()
            )));
        }
        entries.push((i, rel));
    }
    Ok(ExtractPlan { entries, summary })
}

/// Turn a decoded entry name into a safe relative path. A backslash is a
/// separator only now, after decoding (in Big5 it can be half of a
/// character). Refuses NUL, absolute paths, drive prefixes and `..`.
fn decoded_rel_path(name: &str) -> Result<PathBuf> {
    let unified = name.replace('\\', "/");
    let refuse = || cfg_err(format!("拒絕不安全的 zip 條目（zip-slip）：{}", name.escape_debug()));
    if unified.contains('\0') || unified.starts_with('/') {
        return Err(refuse());
    }
    let parts: Vec<&str> = unified.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() {
        return Err(refuse());
    }
    if parts.contains(&"..") || parts.iter().any(|p| !is_portable_component(p)) {
        return Err(refuse());
    }
    let rel: PathBuf = parts.iter().collect();
    if !is_safe_relative(&rel) {
        return Err(refuse());
    }
    Ok(rel)
}

/// A path component that means the same thing on every platform the
/// gateway runs on. Refused everywhere (not only on Windows), so an archive
/// cannot behave differently depending on the host:
/// - `:` (drive prefixes, NTFS alternate data streams `a.md:stream`);
/// - a trailing `.` or space (Windows strips them, so `x.md.` aliases `x.md`);
/// - the reserved device names `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
///   `LPT1`–`LPT9`, case-insensitive, with or without an extension.
fn is_portable_component(c: &str) -> bool {
    if c.contains(':') || c.ends_with('.') || c.ends_with(' ') {
        return false;
    }
    let stem = c.split('.').next().unwrap_or(c).trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !reserved
}

/// A relative path is safe iff it contains only `Normal` components (no `..`,
/// no root, no prefix, no cur-dir games that resolve outward).
fn is_safe_relative(rel: &Path) -> bool {
    if rel.as_os_str().is_empty() {
        return false;
    }
    rel.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Deflate `src_dir` into a `.zip` at `out_path`. Entry names are the paths
/// relative to `src_dir` (forward slashes). Enforces [`MAX_UNPACK_BYTES`] on
/// the total input so `pack` can't be turned into a bomb factory.
pub fn pack_dir(src_dir: &Path, out_path: &Path) -> Result<u64> {
    let file = std::fs::File::create(out_path)
        .map_err(|e| io_err(format!("建立 {} 失敗: {e}", out_path.display())))?;
    let mut zw = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(src_dir, &mut files)?;
    files.sort();

    let mut total: u64 = 0;
    for path in &files {
        let rel = path
            .strip_prefix(src_dir)
            .map_err(|_| cfg_err("內部錯誤：路徑前綴不符".into()))?;
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let data = std::fs::read(path)
            .map_err(|e| io_err(format!("讀取 {} 失敗: {e}", path.display())))?;
        total = total.saturating_add(data.len() as u64);
        if total > MAX_UNPACK_BYTES {
            return Err(cfg_err(format!(
                "打包總量超過上限 {MAX_UNPACK_BYTES} bytes"
            )));
        }
        zw.start_file(name, opts)
            .map_err(|e| cfg_err(format!("zip 寫入失敗: {e}")))?;
        zw.write_all(&data)
            .map_err(|e| io_err(format!("zip 寫入失敗: {e}")))?;
    }
    zw.finish()
        .map_err(|e| cfg_err(format!("zip 收尾失敗: {e}")))?;
    Ok(total)
}

/// Recursively gather regular files (skips symlinks — never packs a link that
/// could point outside the tree).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let rd =
        std::fs::read_dir(dir).map_err(|e| io_err(format!("讀取 {} 失敗: {e}", dir.display())))?;
    for entry in rd {
        let entry = entry.map_err(|e| io_err(format!("讀取目錄項目失敗: {e}")))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| io_err(format!("讀取型別失敗: {e}")))?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            collect_files(&path, out)?;
        } else if ft.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn io_err(msg: String) -> DuDuClawError {
    DuDuClawError::Io(std::io::Error::other(msg))
}
fn cfg_err(msg: String) -> DuDuClawError {
    DuDuClawError::Config(msg)
}

/// Test support shared with the importer's end-to-end tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::Write;

    /// Build a zip whose entry names are the given raw bytes, with bit 11
    /// clear — the shape Windows' built-in compressor (GBK/Big5) and macOS
    /// Archive Utility (UTF-8, unflagged) produce. The writer only takes
    /// `&str`, so each entry is written under a unique ASCII placeholder of
    /// the same length and patched afterwards (local + central headers).
    pub(crate) fn zip_with_raw_names(entries: &[(Vec<u8>, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut placeholders = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            for (i, (raw, body)) in entries.iter().enumerate() {
                let mut ph = format!("~{i}~");
                while ph.len() < raw.len() {
                    ph.push('q');
                }
                assert_eq!(ph.len(), raw.len(), "raw name too short for a placeholder");
                if raw.ends_with(b"/") {
                    ph.pop();
                    ph.push('/');
                    zw.add_directory(ph.clone(), opts).unwrap();
                } else {
                    zw.start_file(ph.clone(), opts).unwrap();
                    zw.write_all(body).unwrap();
                }
                placeholders.push(ph);
            }
            zw.finish().unwrap();
        }
        let mut bytes = buf.into_inner();
        for ((raw, _), ph) in entries.iter().zip(&placeholders) {
            let ph = ph.as_bytes();
            let mut at = 0;
            while let Some(pos) = bytes[at..].windows(ph.len()).position(|w| w == ph) {
                let start = at + pos;
                bytes[start..start + ph.len()].copy_from_slice(raw);
                at = start + ph.len();
            }
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_support::zip_with_raw_names;

    #[test]
    fn safe_relative_rejects_traversal() {
        assert!(is_safe_relative(Path::new("a/b/c.md")));
        assert!(!is_safe_relative(Path::new("../evil")));
        assert!(!is_safe_relative(Path::new("a/../../etc/passwd")));
        assert!(!is_safe_relative(Path::new("/abs")));
        assert!(!is_safe_relative(Path::new("")));
    }

    #[test]
    fn pack_then_extract_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("dc-zip-{}", uuid::Uuid::new_v4()));
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("agents/a")).unwrap();
        std::fs::write(src.join("expert.toml"), "[expert]\nname=\"x\"\n").unwrap();
        std::fs::write(src.join("agents/a/soul.md"), "hello").unwrap();

        let zip_path = tmp.join("out.zip");
        pack_dir(&src, &zip_path).unwrap();
        assert!(zip_path.is_file());

        let dest = tmp.join("dest");
        extract_to(&zip_path, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("agents/a/soul.md")).unwrap(),
            "hello"
        );
        assert!(dest.join("expert.toml").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn zip_slip_entry_is_rejected() {
        // Hand-craft a zip whose entry name traverses upward.
        let tmp = std::env::temp_dir().join(format!("dc-slip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let zip_path = tmp.join("evil.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            // `..` traversal — the reader must refuse this.
            zw.start_file("../escaped.txt", opts).unwrap();
            zw.write_all(b"pwned").unwrap();
            zw.finish().unwrap();
        }
        let dest = tmp.join("dest");
        let err = extract_to(&zip_path, &dest).unwrap_err();
        assert!(
            format!("{err}").contains("zip-slip") || format!("{err}").contains("不安全"),
            "expected zip-slip rejection, got: {err}"
        );
        // The escaped file must NOT exist next to dest.
        assert!(!tmp.join("escaped.txt").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// WP-4G: a single hyper-compressible entry is refused on ratio, before
    /// the 50 MB byte budget would ever notice it.
    #[test]
    fn high_compression_ratio_entry_is_rejected() {
        let tmp = std::env::temp_dir().join(format!("dc-ratio-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let zip_path = tmp.join("bomb.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("payload.bin", opts).unwrap();
            zw.write_all(&vec![0u8; 4 * 1024 * 1024]).unwrap();
            zw.finish().unwrap();
        }
        // The archive on disk stays tiny — that is the whole point of the attack.
        assert!(std::fs::metadata(&zip_path).unwrap().len() < 64 * 1024);

        let dest = tmp.join("dest");
        let err = extract_to(&zip_path, &dest).unwrap_err();
        assert!(
            format!("{err}").contains("壓縮比"),
            "expected a compression-ratio rejection, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn extract_bytes(bytes: &[u8]) -> (tempfile::TempDir, Result<ExtractSummary>) {
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("in.zip");
        std::fs::write(&zip_path, bytes).unwrap();
        let dest = tmp.path().join("out");
        let res = extract_to(&zip_path, &dest);
        (tmp, res)
    }

    /// The distributor's Doubao zip: unflagged UTF-8 CJK names plus a
    /// `__MACOSX/` shadow tree. Names come out readable, clutter is skipped.
    #[test]
    fn macos_archive_utf8_names_and_macosx_junk() {
        let entries = vec![
            ("報告顧問/".as_bytes().to_vec(), &b""[..]),
            ("報告顧問/manifest.json".as_bytes().to_vec(), &b"{}"[..]),
            ("__MACOSX/報告顧問/._manifest.json".as_bytes().to_vec(), &b"junk"[..]),
            ("報告顧問/.DS_Store".as_bytes().to_vec(), &b"junk"[..]),
        ];
        let (tmp, res) = extract_bytes(&zip_with_raw_names(&entries));
        let summary = res.unwrap();
        let out = tmp.path().join("out");
        assert!(out.join("報告顧問").join("manifest.json").is_file());
        assert!(!out.join("__MACOSX").exists());
        assert!(!out.join("報告顧問").join(".DS_Store").exists());
        assert_eq!(summary.junk_skipped, 2);
        assert!(summary.legacy_encoding.is_none());
        // Exactly one top-level directory remains — the single-dir descent can fire.
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[test]
    fn gbk_and_big5_names_are_decoded() {
        for (enc, text, expect) in [
            (encoding_rs::GBK, "报告顾问/技能说明.md", super::super::zip_names::LegacyEncoding::Gbk),
            (encoding_rs::BIG5, "報告顧問/技能說明.md", super::super::zip_names::LegacyEncoding::Big5),
        ] {
            let (raw, _, err) = enc.encode(text);
            assert!(!err);
            let entries = vec![(raw.into_owned(), &b"x"[..])];
            let (tmp, res) = extract_bytes(&zip_with_raw_names(&entries));
            let summary = res.unwrap();
            assert_eq!(summary.legacy_encoding, Some(expect));
            assert!(tmp.path().join("out").join(text).is_file(), "{text}");
        }
    }

    /// CP437-era bytes that are no Chinese encoding: refused, nothing written.
    #[test]
    fn undecodable_names_refuse_the_archive() {
        let entries = vec![(vec![b'c', b'a', b'f', 0x82, b'.', b't', b'x', b't'], &b"x"[..])];
        let (tmp, res) = extract_bytes(&zip_with_raw_names(&entries));
        let err = format!("{}", res.unwrap_err());
        assert!(err.contains("無法辨識編碼"), "{err}");
        assert!(!tmp.path().join("out").exists() || std::fs::read_dir(tmp.path().join("out")).unwrap().count() == 0);
    }

    #[test]
    fn decoded_backslash_and_drive_paths_are_refused() {
        assert!(decoded_rel_path("a\\..\\..\\evil").is_err());
        assert!(decoded_rel_path("C:/Windows/x").is_err());
        assert!(decoded_rel_path("/etc/passwd").is_err());
        assert_eq!(decoded_rel_path("a\\b.md").unwrap(), PathBuf::from("a").join("b.md"));
        // Portable names only: colons, trailing dots/spaces and Windows
        // device names are refused on every platform.
        for bad in ["時間:10.md", "a/b.md:stream", "x.md.", "x.md ", "CON", "aux.txt", "dir/Com1.md", "lpt9"] {
            assert!(decoded_rel_path(bad).is_err(), "{bad}");
        }
        for ok in ["console.md", "com10.md", "lpt.md", "a.b/c.md", "時間-10.md"] {
            assert!(decoded_rel_path(ok).is_ok(), "{ok}");
        }
    }

    /// L1: two entries differing only in case are refused (fail closed).
    #[test]
    fn case_only_duplicate_paths_refuse_the_archive() {
        let entries = vec![
            (b"pack/SKILL.md".to_vec(), &b"a"[..]),
            (b"pack/skill.md".to_vec(), &b"b"[..]),
        ];
        let (_tmp, res) = extract_bytes(&zip_with_raw_names(&entries));
        let err = format!("{}", res.unwrap_err());
        assert!(err.contains("大小寫"), "{err}");
    }

    /// Traversal hidden inside a legacy-encoded name: `..` and an absolute
    /// path survive decoding as ASCII and are refused; a Big5 character whose
    /// second byte is 0x5C (`\`) decodes to the character, not a separator.
    #[test]
    fn gbk_and_big5_names_cannot_traverse() {
        for (enc, text) in [
            (encoding_rs::GBK, "战略/../../逃逸.md"),
            (encoding_rs::BIG5, "戰略/../../逃逸.md"),
            (encoding_rs::BIG5, "/絕對路徑.md"),
        ] {
            let (raw, _, err) = enc.encode(text);
            assert!(!err);
            let entries = vec![(raw.into_owned(), &b"x"[..])];
            let (tmp, res) = extract_bytes(&zip_with_raw_names(&entries));
            assert!(res.is_err(), "{text} must be refused");
            assert!(!tmp.path().join("逃逸.md").exists());
        }
        // 「許」in Big5 is 0xB3 0x5C: the trail byte is a backslash.
        let (raw, _, err) = encoding_rs::BIG5.encode("許功蓋/說明.md");
        assert!(!err);
        assert!(raw.contains(&b'\\'));
        let entries = vec![(raw.into_owned(), &b"x"[..])];
        let (tmp, res) = extract_bytes(&zip_with_raw_names(&entries));
        res.unwrap();
        assert!(tmp.path().join("out").join("許功蓋").join("說明.md").is_file());
    }
}
