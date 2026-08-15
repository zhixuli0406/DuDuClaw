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

/// Extract `zip_path` into `dest_root` (which is created). Every entry is
/// contained under `dest_root`; a traversing entry aborts the whole extraction
/// (fail-closed — a partially-trusted tree is never left on disk on error, the
/// caller extracts into a throwaway temp dir).
pub fn extract_to(zip_path: &Path, dest_root: &Path) -> Result<()> {
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

    std::fs::create_dir_all(dest_root)
        .map_err(|e| io_err(format!("建立 {} 失敗: {e}", dest_root.display())))?;
    // Canonical fence — every written path must stay under this.
    let root_canon = dest_root
        .canonicalize()
        .map_err(|e| io_err(format!("canonicalize {} 失敗: {e}", dest_root.display())))?;

    // WP-4G ceiling 1 — entry count. A million micro-entries exhausts inodes
    // and file handles long before the byte budget notices anything.
    if archive.len() as u64 > doclimits::DEFAULT_MAX_ENTRIES as u64 {
        return Err(cfg_err(format!(
            "壓縮檔條目數 {} 超過上限 {}（疑似耗盡型攻擊）",
            archive.len(),
            doclimits::DEFAULT_MAX_ENTRIES
        )));
    }

    // `total` tracks the sizes the archive *declares*; `written` tracks what
    // actually lands on disk. Both are needed: a declared-size bomb is caught
    // early and cheaply by `total`, and a lying header (declares tiny, inflates
    // huge) is caught only by `written` — the gap WP-4G closed here.
    let mut total: u64 = 0;
    let mut written: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| cfg_err(format!("zip 條目 {i} 讀取失敗: {e}")))?;

        // `enclosed_name` already rejects `..` and absolute paths; we add our
        // own component scan + post-join canonical fence as defence in depth.
        let rel = match entry.enclosed_name() {
            Some(p) => p,
            None => {
                return Err(cfg_err(format!(
                    "拒絕不安全的 zip 條目（zip-slip）：{}",
                    entry.name()
                )));
            }
        };
        if !is_safe_relative(&rel) {
            return Err(cfg_err(format!(
                "拒絕不安全的 zip 條目（zip-slip）：{}",
                rel.display()
            )));
        }

        let out_path = dest_root.join(&rel);
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

    Ok(())
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
    let opts: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

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

#[cfg(test)]
mod tests {
    use super::*;

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
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
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
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
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
}
