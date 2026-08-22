// WP-C-M3 — extract + atomically swap the running `DuDuClaw.app` bundle.
//
// The CLI channel's own self-update (`duduclaw-gateway/src/updater.rs`)
// swaps a SINGLE FILE (the `duduclaw` binary) — rename current binary aside,
// rename the new one into place. That file explicitly REFUSES to do the
// same for a Tauri desktop sidecar (`InstallMethod::Desktop` branch:
// "桌面版由 DuDuClaw App 自動更新...此處不執行覆寫") because overwriting one
// file inside a signed `.app` bundle invalidates the bundle's code-signature
// seal (`_CodeSignature/CodeResource`s is a manifest of every file's hash).
// This module exists because gpui has no bundled updater to defer to the
// way the Tauri shell does — so the whole strategy has to change: distribute
// and swap the ENTIRE `.app` directory atomically (the same approach
// Sparkle/Tauri's own updaters use for macOS), never patch a file inside a
// running signed bundle.
//
// `fs::rename` on the SAME filesystem is atomic and safe even while a
// process is executing the binary underneath it (POSIX keeps the open
// inode alive; the running process is unaffected until its next exec) —
// this is the same guarantee the CLI updater's single-file swap already
// relies on, just applied to a directory instead of a file. Every rename in
// this module is deliberately kept within ONE parent directory
// (`current_app.parent()`) so it can never cross a filesystem boundary
// (`rename(2)` returns `EXDEV` across devices — extracting straight into
// `std::env::temp_dir()`, which is not guaranteed to share a volume with
// `/Applications`, would risk exactly that failure mode on a multi-volume
// Mac).

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Matches `duduclaw-gateway/src/updater.rs::MAX_DOWNLOAD_BYTES` — same
/// ceiling, same rationale (a legitimate release archive is nowhere near
/// this size; anything past it is either a mistake or a zip-bomb-style
/// attack).
pub const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;

/// Resolve the currently-running `.app` bundle root from an executable path
/// shaped like `.../<Name>.app/Contents/MacOS/<bin>`. Returns a clear error
/// (never panics) for anything else — notably a dev build launched directly
/// from `target/release/`, which has no bundle to update at all.
pub fn running_app_bundle_path(exe: &Path) -> Result<PathBuf, String> {
    let macos_dir = exe.parent().ok_or("無法解析執行檔所在目錄")?;
    let contents_dir = macos_dir.parent().ok_or("無法解析 .app 目錄結構")?;
    let app_dir = contents_dir.parent().ok_or("無法解析 .app 目錄結構")?;

    let is_macos_dir = macos_dir.file_name().and_then(|n| n.to_str()) == Some("MacOS");
    let is_contents_dir = contents_dir.file_name().and_then(|n| n.to_str()) == Some("Contents");
    let is_app_dir = app_dir.extension().and_then(|e| e.to_str()) == Some("app");

    if !(is_macos_dir && is_contents_dir && is_app_dir) {
        return Err(
            "目前的執行檔不在標準的 .app 目錄結構下（可能是直接執行開發版 binary），無法自動更新".to_string(),
        );
    }
    Ok(app_dir.to_path_buf())
}

/// Extract a tar.gz archive's top-level `<Name>.app` directory into
/// `dest_parent`, returning the extracted bundle's path. Hardened the same
/// way `duduclaw-gateway/src/updater.rs::extract_from_tar_gz` is: symlink
/// and hard-link entries are skipped, absolute paths and `..` traversal
/// components are skipped, and total extracted size is capped. Additionally
/// (specific to this being a whole DIRECTORY rather than one file): every
/// entry's first path component must be the SAME `<Name>.app` — an archive
/// that smuggled a second top-level entry (e.g. a sibling file dropped next
/// to the bundle, or a component of the wrong app name entirely) is
/// rejected outright rather than silently accepted alongside the bundle.
pub fn extract_app_bundle(archive_bytes: &[u8], dest_parent: &Path) -> Result<PathBuf, String> {
    if archive_bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(format!("更新封存檔過大: {} bytes（上限 {MAX_ARCHIVE_BYTES}）", archive_bytes.len()));
    }

    let gz = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(gz);

    let mut app_name: Option<String> = None;
    let mut extracted_bytes: u64 = 0;

    let entries = archive.entries().map_err(|e| format!("更新封存檔格式錯誤: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("封存檔項目讀取錯誤: {e}"))?;

        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            continue; // rejected — same policy as the CLI updater's extractor
        }

        let path = entry.path().map_err(|e| format!("封存檔內路徑錯誤: {e}"))?.into_owned();
        if is_unsafe_entry_path(&path) {
            continue; // rejected — path traversal / absolute path
        }
        if is_apple_double_entry(&path) {
            continue; // macOS resource-fork/xattr shadow entry — see `is_apple_double_entry`'s doc comment
        }

        let Some(Component::Normal(first)) = path.components().next() else { continue };
        let Some(first_str) = first.to_str() else { continue }; // non-UTF-8 name — reject silently, same as an unsafe entry
        if !first_str.ends_with(".app") {
            continue; // anything outside the bundle root is not part of the update
        }
        match &app_name {
            Some(name) if name != first_str => {
                return Err("更新封存檔內含超過一個 .app 目錄，拒絕安裝".to_string());
            }
            Some(_) => {}
            None => app_name = Some(first_str.to_string()),
        }

        let size = entry.size();
        extracted_bytes = extracted_bytes.saturating_add(size);
        if extracted_bytes > MAX_ARCHIVE_BYTES {
            return Err("解壓後大小超過上限（疑似異常封存檔），拒絕安裝".to_string());
        }

        let out_path = dest_parent.join(&path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("建立目錄失敗 {}: {e}", parent.display()))?;
        }
        entry.unpack(&out_path).map_err(|e| format!("解壓 {} 失敗: {e}", out_path.display()))?;
    }

    let app_name = app_name.ok_or("更新封存檔內找不到 .app 目錄")?;
    Ok(dest_parent.join(app_name))
}

/// True for any archive-entry path that must never be extracted verbatim:
/// absolute paths, or any `..`/root component anywhere in the path. Pulled
/// out as a pure, `Path`-level predicate (rather than inlined in the loop
/// above) specifically so it's unit-testable directly with a crafted
/// `Path::new("../../etc/evil")` — the `tar` crate's own `Header::set_path`
/// refuses to ENCODE a `..`-bearing path through its safe builder API, so a
/// round-trip-through-a-real-archive test can only ever exercise the
/// absolute-path branch; a hand-crafted malicious tar stream (built by
/// something other than this crate's own `tar::Builder`) is under no such
/// obligation, so the actual defense — this predicate — needs to be
/// verifiable independent of what the `tar` crate's writer will let a test
/// construct.
fn is_unsafe_entry_path(path: &Path) -> bool {
    path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir))
}

/// True for a macOS AppleDouble resource-fork/xattr shadow entry (any path
/// component named `._<name>`) — found the hard way (2026-08-22 live
/// verification, `updater::tests::live_fake_update_wrong_signature_
/// rejected_correct_signature_installs`): macOS's system `tar` writes a
/// SEPARATE `._<name>` sidecar entry for every file/directory that carries
/// extended attributes (this crate's own ad-hoc-`codesign`'d fixture bundle
/// picked up `com.apple.provenance` just from being created on macOS — the
/// SAME will be true of a real notarized `.app` produced by CI), including
/// a TOP-LEVEL `._DuDuClaw.app` entry. That entry's `.ends_with(".app")`
/// matched the bundle-root check above and, having a different literal name
/// than the real `DuDuClaw.app`, tripped the "more than one .app directory"
/// rejection outright — turning routine macOS tar behavior into every
/// update being refused. `COPYFILE_DISABLE=1` on the CI packaging step
/// (`.github/workflows/native-gui-desktop-release.yml`) prevents these at
/// the SOURCE, but this extractor filters them defensively too — a correct,
/// harmless macOS tar convention should degrade to "silently skipped
/// clutter", not a hard failure, regardless of which machine produced the
/// archive.
fn is_apple_double_entry(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::Normal(name) if name.to_str().is_some_and(|s| s.starts_with("._"))))
}

/// Structural + code-signature sanity check on a freshly-extracted bundle,
/// run BEFORE it is swapped into place. Deliberately NOT a spawn-and-check
/// smoke test the way the CLI updater verifies its single-file binary
/// (`apply_update_inner`'s `Command::new(&tmp_for_verify).arg("version")`):
/// `duduclaw-native-gui`'s `main()` has no CLI-args fast-exit path at all —
/// it unconditionally opens a gpui window and starts a persistent run loop,
/// so "spawn it and check the exit code" would pop a second visible window
/// and never return, which is worse than not checking. `codesign --verify`
/// is the macOS-native equivalent check for a `.app` bundle specifically:
/// it validates the bundle's `_CodeSignature` manifest against every file
/// actually on disk (catches a truncated/corrupted extraction the same way
/// a spawn-and-check would, without launching anything) — the exact same
/// call `scripts/desktop/sign-notarize-macos.sh` already uses to verify a
/// freshly-signed bundle in CI (`codesign --verify --strict --verbose=2`).
/// `--deep` additionally walks any nested bundle content (this crate's own
/// bundle has none today, but a future one might).
pub fn verify_bundle_signature(app_path: &Path) -> Result<(), String> {
    if !app_path.join("Contents").join("Info.plist").exists() {
        return Err("新版本的 .app 結構不完整（缺少 Info.plist），拒絕安裝".to_string());
    }
    let output = std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app_path)
        .output()
        .map_err(|e| format!("無法執行 codesign 驗證: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("新版本的程式碼簽章驗證失敗，拒絕安裝: {}", stderr.trim()));
    }
    Ok(())
}

/// Atomically swap `current_app` for `new_app` — both MUST share the same
/// parent directory (same filesystem), enforced by the caller
/// ([`stage_dir_for`]). Backs up the current bundle first so a failed final
/// rename can roll back; cleans the backup up on success. Every failure
/// path is reported, never silently swallowed — a rollback failure returns
/// an explicit "manual recovery needed" message rather than pretending to
/// have succeeded (mirrors `duduclaw-gateway/src/updater.rs::apply_update_
/// inner`'s CRITICAL rollback-failure branch).
pub fn swap_app_bundle(current_app: &Path, new_app: &Path) -> Result<(), String> {
    let mut backup_os = current_app.as_os_str().to_owned();
    backup_os.push(".bak");
    let backup_path = PathBuf::from(backup_os);

    if backup_path.exists() {
        let _ = fs::remove_dir_all(&backup_path);
    }

    fs::rename(current_app, &backup_path)
        .map_err(|e| format!("備份目前版本失敗: {e}"))?;

    match fs::rename(new_app, current_app) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&backup_path);
            Ok(())
        }
        Err(e) => match fs::rename(&backup_path, current_app) {
            Ok(()) => Err(format!("安裝新版本失敗，已回滾至原版本: {e}")),
            Err(rollback_err) => Err(format!(
                "安裝新版本失敗，且回滾也失敗: {e}。原版本備份仍在 {}，需要手動處理: {rollback_err}",
                backup_path.display()
            )),
        },
    }
}

/// A hidden staging directory guaranteed to share a filesystem with
/// `current_app` (its sibling, same parent) — see this module's header
/// comment on why every rename must stay within one directory. Caller is
/// responsible for removing it once the extracted bundle has either been
/// swapped into place or the whole attempt has been abandoned.
pub fn stage_dir_for(current_app: &Path, unique_suffix: &str) -> Result<PathBuf, String> {
    let parent = current_app.parent().ok_or("無法解析 .app 所在目錄")?;
    let dir = parent.join(format!(".duduclaw-native-gui-update-{unique_suffix}"));
    fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "無法在 {} 建立暫存目錄（可能沒有寫入權限）: {e}",
            parent.display()
        )
    })?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ddc-ng-updater-install-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a minimal tar.gz containing exactly `entries` (path, is_dir,
    /// content) — used to construct both legitimate and malicious fixture
    /// archives without needing a real `.app` on disk.
    fn build_tar_gz(entries: &[(&str, bool, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, is_dir, content) in entries {
            if *is_dir {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append(&header, std::io::empty()).unwrap();
            } else {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, *content).unwrap();
            }
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn running_app_bundle_path_resolves_the_standard_layout() {
        let exe = Path::new("/Applications/DuDuClaw.app/Contents/MacOS/duduclaw-native-gui");
        assert_eq!(running_app_bundle_path(exe).unwrap(), Path::new("/Applications/DuDuClaw.app"));
    }

    #[test]
    fn running_app_bundle_path_rejects_a_dev_build_layout() {
        let exe = Path::new("/Users/dev/DuDuClaw/crates/duduclaw-native-gui/target/release/duduclaw-native-gui");
        assert!(running_app_bundle_path(exe).is_err());
    }

    #[test]
    fn extract_app_bundle_happy_path() {
        let archive = build_tar_gz(&[
            ("DuDuClaw.app/", true, b""),
            ("DuDuClaw.app/Contents/", true, b""),
            ("DuDuClaw.app/Contents/Info.plist", false, b"<plist/>"),
            ("DuDuClaw.app/Contents/MacOS/", true, b""),
            ("DuDuClaw.app/Contents/MacOS/duduclaw-native-gui", false, b"fake binary bytes"),
        ]);
        let dest = temp_dir("happy");
        let app_path = extract_app_bundle(&archive, &dest).unwrap();
        assert_eq!(app_path, dest.join("DuDuClaw.app"));
        assert!(app_path.join("Contents/Info.plist").exists());
        assert!(app_path.join("Contents/MacOS/duduclaw-native-gui").exists());
        let _ = fs::remove_dir_all(&dest);
    }

    /// The `tar` crate's own `Header::set_path` refuses to even ENCODE a
    /// `..`-bearing path through its safe builder API (verified: an earlier
    /// version of this test tried to build such a fixture and the *builder*
    /// panicked before the archive ever reached this crate's extractor) —
    /// so the traversal defense is tested directly against the pure
    /// predicate below, and this test instead proves the OTHER half
    /// (absolute paths) round-trips correctly through a real archive.
    #[test]
    fn extract_app_bundle_rejects_absolute_path_entries() {
        let archive = build_tar_gz(&[
            ("DuDuClaw.app/", true, b""),
            ("DuDuClaw.app/Contents/Info.plist", false, b"<plist/>"),
        ]);
        let dest = temp_dir("absolute-path");
        let app_path = extract_app_bundle(&archive, &dest).unwrap();
        assert!(app_path.join("Contents/Info.plist").exists());
        assert!(is_unsafe_entry_path(Path::new("/etc/evil")));
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn is_unsafe_entry_path_rejects_traversal_and_absolute_paths() {
        assert!(is_unsafe_entry_path(Path::new("../../etc/evil")));
        assert!(is_unsafe_entry_path(Path::new("DuDuClaw.app/../../../etc/evil")));
        assert!(is_unsafe_entry_path(Path::new("/etc/evil")));
        assert!(!is_unsafe_entry_path(Path::new("DuDuClaw.app/Contents/Info.plist")));
    }

    #[test]
    fn is_apple_double_entry_matches_at_any_depth() {
        assert!(is_apple_double_entry(Path::new("._DuDuClaw.app")));
        assert!(is_apple_double_entry(Path::new("DuDuClaw.app/Contents/._MacOS")));
        assert!(is_apple_double_entry(Path::new("DuDuClaw.app/Contents/MacOS/._duduclaw-native-gui")));
        assert!(!is_apple_double_entry(Path::new("DuDuClaw.app/Contents/Info.plist")));
    }

    /// Regression guard for the bug this crate's own live end-to-end test
    /// (`updater::tests::live_fake_update_wrong_signature_rejected_correct_
    /// signature_installs`) caught in the wild (2026-08-22): a top-level
    /// `._DuDuClaw.app` AppleDouble sidecar entry — which macOS's system
    /// `tar` writes automatically for any code-signed bundle unless
    /// `COPYFILE_DISABLE=1` is set — used to be misidentified as a SECOND,
    /// different `.app` directory and rejected the whole archive outright.
    #[test]
    fn extract_app_bundle_ignores_a_top_level_apple_double_sidecar_entry() {
        let archive = build_tar_gz(&[
            ("._DuDuClaw.app", false, b"AppleDouble resource fork bytes, irrelevant content"),
            ("DuDuClaw.app/Contents/Info.plist", false, b"<plist/>"),
            ("DuDuClaw.app/Contents/MacOS/._duduclaw-native-gui", false, b"AppleDouble sidecar for the binary"),
            ("DuDuClaw.app/Contents/MacOS/duduclaw-native-gui", false, b"real binary bytes"),
        ]);
        let dest = temp_dir("apple-double");
        let app_path = extract_app_bundle(&archive, &dest).unwrap();
        assert!(app_path.join("Contents/Info.plist").exists());
        assert!(app_path.join("Contents/MacOS/duduclaw-native-gui").exists());
        assert!(!dest.join("._DuDuClaw.app").exists());
        assert!(!app_path.join("Contents/MacOS/._duduclaw-native-gui").exists());
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_app_bundle_rejects_two_different_top_level_app_dirs() {
        let archive = build_tar_gz(&[
            ("DuDuClaw.app/Contents/Info.plist", false, b"<plist/>"),
            ("Evil.app/Contents/Info.plist", false, b"<plist/>"),
        ]);
        let dest = temp_dir("two-apps");
        assert!(extract_app_bundle(&archive, &dest).is_err());
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_app_bundle_ignores_entries_outside_any_app_bundle() {
        let archive = build_tar_gz(&[
            ("DuDuClaw.app/Contents/Info.plist", false, b"<plist/>"),
            ("README.txt", false, b"not part of the bundle"),
        ]);
        let dest = temp_dir("stray-file");
        let app_path = extract_app_bundle(&archive, &dest).unwrap();
        assert!(app_path.join("Contents/Info.plist").exists());
        assert!(!dest.join("README.txt").exists());
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_app_bundle_rejects_oversized_archive_bytes() {
        let huge = vec![0u8; (MAX_ARCHIVE_BYTES + 1) as usize];
        assert!(extract_app_bundle(&huge, Path::new("/tmp")).is_err());
    }

    #[test]
    fn extract_app_bundle_missing_app_dir_is_a_clean_error() {
        let archive = build_tar_gz(&[("not_an_app/file.txt", false, b"hi")]);
        let dest = temp_dir("no-app");
        assert!(extract_app_bundle(&archive, &dest).is_err());
        let _ = fs::remove_dir_all(&dest);
    }

    /// The atomic-swap mechanics, exercised against plain stand-in
    /// directories (not a real signed `.app`) — same "honest substitution"
    /// testing approach `sidecar.rs`'s own tests already use for a stub
    /// `duduclaw` binary instead of the real one. What's under test here is
    /// the rename/backup/rollback filesystem choreography itself, which is
    /// identical regardless of what's inside the directories.
    #[test]
    fn swap_app_bundle_replaces_current_with_new_and_cleans_up_backup() {
        let root = temp_dir("swap-happy");
        let current = root.join("DuDuClaw.app");
        let new = root.join(".staging").join("DuDuClaw.app");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("marker.txt"), "old").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("marker.txt"), "new").unwrap();

        swap_app_bundle(&current, &new).unwrap();

        assert_eq!(fs::read_to_string(current.join("marker.txt")).unwrap(), "new");
        let mut backup_os = current.as_os_str().to_owned();
        backup_os.push(".bak");
        assert!(!PathBuf::from(backup_os).exists(), "backup should be cleaned up after a successful swap");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_app_bundle_rolls_back_when_the_new_bundle_vanishes_mid_swap() {
        let root = temp_dir("swap-rollback");
        let current = root.join("DuDuClaw.app");
        let missing_new = root.join(".staging").join("DoesNotExist.app");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("marker.txt"), "old").unwrap();

        let err = swap_app_bundle(&current, &missing_new).unwrap_err();
        assert!(err.contains("回滾"), "expected a rollback message, got: {err}");
        assert!(current.exists(), "current bundle must be restored after a failed swap");
        assert_eq!(fs::read_to_string(current.join("marker.txt")).unwrap(), "old");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stage_dir_for_is_a_sibling_of_current_app() {
        let root = temp_dir("stage-dir");
        let current = root.join("DuDuClaw.app");
        fs::create_dir_all(&current).unwrap();
        let staged = stage_dir_for(&current, "test123").unwrap();
        assert_eq!(staged.parent(), current.parent());
        assert!(staged.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_bundle_signature_rejects_a_bundle_with_no_info_plist() {
        let dest = temp_dir("no-plist");
        let app = dest.join("Bad.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        assert!(verify_bundle_signature(&app).is_err());
        let _ = fs::remove_dir_all(&dest);
    }
}
