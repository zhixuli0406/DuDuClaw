// WP-C-M3 — self-update client for the `DuDuClaw.app` bundle itself.
//
// This is a DIFFERENT update channel from the one `screens/about.rs`
// already surfaces via `system.check_update`/`system.apply_update`: those
// two RPCs update the local GATEWAY SIDECAR process (`duduclaw`, a separate
// binary this app spawns/attaches to — see `sidecar.rs`), self-updated by
// `crates/duduclaw-gateway/src/updater.rs`. This module updates the GUI
// SHELL app that hosts the whole window — a second, independent piece of
// software with its own release cadence (`native-gui-v*` tags, see
// `.github/workflows/native-gui-desktop-release.yml`), which had NO update
// mechanism at all before this pass (gpui has no bundled webview to run the
// Tauri shell's `tauri-plugin-updater` the way `src-tauri` does).
//
// ── Architecture: same shape as `sidecar::SidecarManager`, not
// `ws_status`'s channel-actor ────────────────────────────────────────────
// `main.rs`'s module doc comment explains why anything using tokio/reqwest
// needs its own dedicated OS thread + tiny current-thread runtime (gpui's
// own executor is not a tokio context). `ws_status.rs`/`chat_ws.rs` solve
// that with a long-lived background thread + `Command`/`Event` mpsc
// channels, polled every 100ms from `main.rs`'s foreground loop — the right
// shape for a PERSISTENT connection with many small round trips. A version
// check + an install are the opposite: rare, one-shot, no persistent
// connection to maintain. `sidecar.rs`'s own header comment makes exactly
// this call for its "low-frequency housekeeping tasks" ("none of which need
// tokio's reactor — pulling in tokio... just to avoid three
// `std::thread::spawn` calls would be the actual yak-shave") and lands on a
// plain `Arc<Self> { Mutex<Status> }` read from the UI thread, mutated by
// short-lived background threads. This module follows that same precedent:
// [`UpdaterManager`] is a `Mutex<UpdaterStatus>` behind an `Arc`, `check()`/
// `install()` each spawn ONE short-lived thread (which builds its own tiny
// tokio runtime for the `reqwest` calls inside, same as `ws_status::spawn`
// does for its long-lived one) and exit when done — no channel, no
// `main.rs` wiring beyond a periodic `cx.notify()` while the About page is
// visible (mirroring the EXACT precedent `main.rs`'s poll loop already
// established for `sidecar`'s "gatewayPicker" page).

pub mod install;
pub mod manifest;
pub mod verify;

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Same back-off ladder `duduclaw-gateway/src/updater.rs::DOWNLOAD_RETRY_
/// DELAYS_SECS` uses for the CLI channel — reused as a design choice (a
/// flaky download deserves the same patience regardless of which channel
/// it's downloading for), not shared code (this crate cannot depend on
/// `duduclaw-gateway`, see `Cargo.toml`'s header comment).
const DOWNLOAD_RETRY_DELAYS_SECS: [u64; 2] = [5, 15];

/// This app's self-update state machine. `Available`/`Downloading` carry
/// enough for the UI to render without a second round trip back into this
/// module — `screens/about.rs` reads this via `RootView::app_updater.
/// status()`, the same synchronous-Mutex-read shape `sidecar::SidecarManager
/// ::status()` already established.
#[derive(Debug, Clone)]
pub enum UpdaterStatus {
    /// Nothing checked yet this launch.
    Idle,
    Checking,
    UpToDate,
    Available { version: String, notes: String, download_url: String },
    Downloading { attempt: u32, max_attempts: u32 },
    /// Verified archive is being extracted and swapped into place — a
    /// distinct state from `Downloading` so the UI can say "安裝中" instead
    /// of implying network activity is still happening.
    Installing,
    /// Swap succeeded — the NEW `.app` is on disk, but this still-running
    /// process is the OLD binary in memory. A relaunch (user-triggered; see
    /// `screens/about.rs`'s restart prompt) is required to actually run it.
    ReadyToRestart { version: String },
    Failed { message: String },
}

/// Whether a `check()`/`install()` call should be a no-op because one is
/// already in flight. Pulled out as a pure function (rather than inlined
/// into `UpdaterManager::check`/`install`) so the guard logic is unit-
/// testable without spawning a real background thread.
fn is_busy(status: &UpdaterStatus) -> bool {
    matches!(status, UpdaterStatus::Checking | UpdaterStatus::Downloading { .. } | UpdaterStatus::Installing)
}

pub struct UpdaterManager {
    status: Mutex<UpdaterStatus>,
}

impl UpdaterManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { status: Mutex::new(UpdaterStatus::Idle) })
    }

    pub fn status(&self) -> UpdaterStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, s: UpdaterStatus) {
        *self.status.lock().unwrap() = s;
    }

    /// This app's own baked-in version — re-exported here so
    /// `screens/about.rs` doesn't need to reach into the `manifest`
    /// submodule directly for the one thing it actually renders
    /// unconditionally (the version badge).
    pub fn app_version(&self) -> &'static str {
        manifest::current_version()
    }

    /// Fire a background version check against [`manifest::MANIFEST_URL`].
    /// No-op while a check or install is already in flight — mirrors
    /// `SidecarManager::start`'s idempotent-while-in-flight guard.
    pub fn check(self: &Arc<Self>) {
        if is_busy(&self.status()) {
            return;
        }
        self.set_status(UpdaterStatus::Checking);
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let rt = match new_runtime() {
                Ok(rt) => rt,
                Err(e) => {
                    me.set_status(UpdaterStatus::Failed { message: e });
                    return;
                }
            };
            let outcome = rt.block_on(check_once());
            me.set_status(match outcome {
                Ok(status) => status,
                Err(message) => UpdaterStatus::Failed { message },
            });
        });
    }

    /// Download, verify, extract, and swap the update found by the last
    /// [`check`](Self::check) — only meaningful while `status() ==
    /// Available { .. }`; a no-op otherwise (the UI only ever renders the
    /// install button in that state, so this is a defensive guard, not the
    /// primary gate).
    pub fn install(self: &Arc<Self>) {
        let (version, download_url) = match self.status() {
            UpdaterStatus::Available { version, download_url, .. } => (version, download_url),
            _ => return,
        };
        let max_attempts = DOWNLOAD_RETRY_DELAYS_SECS.len() as u32 + 1;
        self.set_status(UpdaterStatus::Downloading { attempt: 1, max_attempts });
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let rt = match new_runtime() {
                Ok(rt) => rt,
                Err(e) => {
                    me.set_status(UpdaterStatus::Failed { message: e });
                    return;
                }
            };
            let outcome = rt.block_on(install_once(&me, &download_url));
            me.set_status(match outcome {
                Ok(()) => UpdaterStatus::ReadyToRestart { version },
                Err(message) => UpdaterStatus::Failed { message },
            });
        });
    }
}

/// Spawn a fresh copy of THIS process's own binary path, detached from the
/// current process. Used by `screens/about.rs`'s "重新啟動套用" button after
/// [`UpdaterStatus::ReadyToRestart`]: `fs::rename` in `install::
/// swap_app_bundle` already replaced the file at this exact path with the
/// new version, but the CURRENTLY RUNNING process is still executing the
/// old bytes (POSIX keeps a renamed-away file's inode alive for a process
/// that already has it open) — spawning `std::env::current_exe()` again
/// launches the NEW binary, since that call resolves the same path string,
/// not a live inode. Does not itself terminate this process — the caller
/// still needs to call `cx.quit()` (a gpui `App` action this module has no
/// access to) once the spawn succeeds.
pub fn relaunch_current_binary() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("無法取得目前執行檔路徑: {e}"))?;
    std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| format!("重新啟動失敗: {e}"))?;
    Ok(())
}

fn new_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("執行環境初始化失敗: {e}"))
}

/// Redirect policy restricted to GitHub's own hosts — same allowlist shape
/// `duduclaw-gateway/src/updater.rs::apply_update_with_progress` uses for
/// its own download client ("release-assets.githubusercontent.com" is
/// GitHub's current Azure-blob-signed redirect target; the legacy
/// "objects.githubusercontent.com" host is tolerated too).
fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("duduclaw-native-gui/{}", manifest::current_version()))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let target = attempt.url().to_string();
            if target.starts_with("https://github.com/")
                || target.starts_with("https://objects.githubusercontent.com/")
                || target.starts_with("https://release-assets.githubusercontent.com/")
            {
                attempt.follow()
            } else {
                attempt.error(format!("重新導向被拒絕（非白名單網域）: {target}"))
            }
        }))
        .build()
        .map_err(|e| format!("HTTP client 建立失敗: {e}"))
}

async fn check_once() -> Result<UpdaterStatus, String> {
    let client = http_client()?;
    let resp =
        client.get(manifest::MANIFEST_URL).send().await.map_err(|e| format!("更新資訊下載失敗: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("更新伺服器回應異常: HTTP {}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| format!("更新資訊讀取失敗: {e}"))?;
    let parsed = manifest::parse_manifest(&body)?;
    if !manifest::is_newer(manifest::current_version(), &parsed.version) {
        return Ok(UpdaterStatus::UpToDate);
    }
    let download_url = manifest::resolve_platform_entry(&parsed)?.url.clone();
    Ok(UpdaterStatus::Available { version: parsed.version, notes: parsed.notes, download_url })
}

/// One asset fetch, classifying whether the failure is worth retrying.
/// `(retryable, message)` rather than a richer error type — this module has
/// exactly one retry site ([`install_once`]'s archive download loop), so a
/// full `FailureClass`/`StageError` pair (as `duduclaw-gateway/src/
/// updater.rs` defines for its own, much larger retry surface) would be
/// over-engineering for one call site.
async fn fetch_bytes(client: &reqwest::Client, url: &str, what: &str) -> Result<Vec<u8>, (bool, String)> {
    let resp = client.get(url).send().await.map_err(|e| (true, format!("{what}下載失敗: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let retryable = status.is_server_error() || matches!(status.as_u16(), 403 | 404 | 408 | 425 | 429);
        return Err((retryable, format!("{what}下載回應異常: HTTP {status}")));
    }
    if let Some(len) = resp.content_length() {
        if len > install::MAX_ARCHIVE_BYTES {
            return Err((false, format!("{what}過大: {len} bytes（上限 {}）", install::MAX_ARCHIVE_BYTES)));
        }
    }
    let bytes = resp.bytes().await.map_err(|e| (true, format!("{what}讀取失敗: {e}")))?;
    if bytes.len() as u64 > install::MAX_ARCHIVE_BYTES {
        return Err((false, format!("{what}超過大小上限")));
    }
    Ok(bytes.to_vec())
}

/// Download → verify → extract → swap. Checksum/signature URLs are DERIVED
/// from `download_url` (`<url>.sha256` / `<url>.minisig`), never read from
/// the manifest payload — same "the payload can point the download
/// somewhere, but never dictate what proves its own integrity" principle
/// `duduclaw-gateway/src/updater.rs`'s `[S1]` comment documents.
async fn install_once(me: &Arc<UpdaterManager>, download_url: &str) -> Result<(), String> {
    if !manifest::is_valid_release_url(download_url) {
        return Err("拒絕不安全的下載位址".to_string());
    }
    let checksum_url = format!("{download_url}.sha256");
    let signature_url = format!("{download_url}.minisig");
    if !manifest::is_valid_release_url(&checksum_url) || !manifest::is_valid_release_url(&signature_url) {
        return Err("拒絕不安全的校驗/簽章位址".to_string());
    }

    let client = http_client()?;
    let max_attempts = DOWNLOAD_RETRY_DELAYS_SECS.len() as u32 + 1;
    let archive_bytes = {
        let mut attempt = 1u32;
        loop {
            me.set_status(UpdaterStatus::Downloading { attempt, max_attempts });
            match fetch_bytes(&client, download_url, "更新封存檔").await {
                Ok(bytes) => break bytes,
                Err((true, msg)) if attempt < max_attempts => {
                    let delay = DOWNLOAD_RETRY_DELAYS_SECS.get((attempt - 1) as usize).copied().unwrap_or(15);
                    eprintln!("[updater] {msg} — retrying in {delay}s ({attempt}/{max_attempts})");
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    attempt += 1;
                }
                Err((_, msg)) => return Err(msg),
            }
        }
    };

    // Integrity gates never retry — a bad checksum/signature is a permanent
    // failure, not a network hiccup (same doctrine `duduclaw-gateway/src/
    // updater.rs`'s `verify_archive_integrity` comment states explicitly:
    // "retrying past a bad checksum or a bad Ed25519 signature would turn a
    // fail-closed security gate into 'try until it slips through'").
    let checksum_text = String::from_utf8_lossy(
        &fetch_bytes(&client, &checksum_url, "校驗檔").await.map_err(|(_, m)| m)?,
    )
    .into_owned();
    let sig_text = String::from_utf8_lossy(
        &fetch_bytes(&client, &signature_url, "簽章檔").await.map_err(|(_, m)| m)?,
    )
    .into_owned();
    verify::verify_archive(&archive_bytes, &checksum_text, &sig_text)?;

    me.set_status(UpdaterStatus::Installing);

    // Filesystem work is blocking (tar/gzip extraction, `codesign` subprocess,
    // directory renames) — run it off this (single-threaded) runtime so it
    // never stalls timers/other IO on the same thread.
    tokio::task::spawn_blocking(move || install_verified_archive(&archive_bytes))
        .await
        .map_err(|e| format!("安裝工作執行失敗: {e}"))?
}

/// Extract + swap, given already-integrity-verified archive bytes. Always
/// cleans up its staging directory, whether it succeeded or not.
fn install_verified_archive(archive_bytes: &[u8]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("無法取得目前執行檔路徑: {e}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let current_app = install::running_app_bundle_path(&exe)?;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stage_dir = install::stage_dir_for(&current_app, &format!("{}-{suffix}", std::process::id()))?;

    let result = (|| {
        let new_app = install::extract_app_bundle(archive_bytes, &stage_dir)?;
        install::verify_bundle_signature(&new_app)?;
        install::swap_app_bundle(&current_app, &new_app)
    })();

    let _ = std::fs::remove_dir_all(&stage_dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_manager_starts_idle() {
        let mgr = UpdaterManager::new();
        assert!(matches!(mgr.status(), UpdaterStatus::Idle));
    }

    #[test]
    fn app_version_matches_cargo_pkg_version() {
        let mgr = UpdaterManager::new();
        assert_eq!(mgr.app_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn is_busy_covers_every_in_flight_state() {
        assert!(is_busy(&UpdaterStatus::Checking));
        assert!(is_busy(&UpdaterStatus::Downloading { attempt: 1, max_attempts: 3 }));
        assert!(is_busy(&UpdaterStatus::Installing));
        assert!(!is_busy(&UpdaterStatus::Idle));
        assert!(!is_busy(&UpdaterStatus::UpToDate));
        assert!(!is_busy(&UpdaterStatus::Available {
            version: "9.9.9".into(),
            notes: String::new(),
            download_url: String::new(),
        }));
        assert!(!is_busy(&UpdaterStatus::ReadyToRestart { version: "9.9.9".into() }));
        assert!(!is_busy(&UpdaterStatus::Failed { message: "x".into() }));
    }

    /// `install()` while NOT `Available` must be a safe no-op (no thread
    /// spawned, status unchanged) — the button that calls it is only
    /// rendered/enabled in that state, but the guard itself must hold even
    /// if a stale click races a state transition.
    #[test]
    fn install_without_an_available_update_is_a_no_op() {
        let mgr = UpdaterManager::new();
        mgr.install();
        assert!(matches!(mgr.status(), UpdaterStatus::Idle));
    }

    /// `check()` while already busy must not reset progress state (e.g. a
    /// double-click on "檢查更新" while a check is already running must not
    /// clobber a `Downloading` install already in flight).
    #[test]
    fn check_while_installing_does_not_clobber_progress() {
        let mgr = UpdaterManager::new();
        mgr.set_status(UpdaterStatus::Downloading { attempt: 2, max_attempts: 3 });
        mgr.check();
        assert!(matches!(mgr.status(), UpdaterStatus::Downloading { attempt: 2, .. }));
    }

    /// Full LOCAL pipeline, live: build a fake `.app` bundle, ad-hoc
    /// `codesign` it, tar+gzip it, sign the archive with a FRESH throwaway
    /// minisign keypair (`minisign -G -W`, never touching the production
    /// key at `~/.minisign/duduclaw-release.key`), then run the exact same
    /// verify → extract → verify-signature → swap sequence
    /// `install_verified_archive` runs internally — first with the correct
    /// signature (must succeed and actually replace the bundle on disk),
    /// then with a tampered one (must be refused before anything is
    /// extracted). This is this task's own "構造一個假 latest.json＋錯誤簽章→
    /// 驗證被拒；正確簽章→驗證通過" acceptance check, exercised end to end
    /// through real `tar`/`flate2`/`minisign-verify`/`codesign`/filesystem
    /// code — not mocked at any layer. `#[ignore]`d for the same reason
    /// `sidecar.rs`'s own live lifecycle test is: it shells out to external
    /// binaries (`minisign`, `codesign`) not guaranteed present on every CI
    /// runner, so it's a manual/local verification tool, not part of the
    /// default `cargo test` run.
    #[test]
    #[ignore = "shells out to `minisign`/`codesign`; run explicitly with `cargo test -- --ignored`"]
    fn live_fake_update_wrong_signature_rejected_correct_signature_installs() {
        use sha2::Digest;
        use std::path::PathBuf;
        use std::process::Command;

        if Command::new("minisign").arg("-v").output().is_err() {
            eprintln!("skipping: `minisign` binary not found on PATH");
            return;
        }
        if Command::new("codesign").arg("-h").output().is_err() {
            eprintln!("skipping: `codesign` binary not found on PATH (non-macOS host)");
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "ddc-ng-updater-live-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // 1. A fake .app bundle, ad-hoc code-signed (`-s -`: no Developer ID
        //    needed, works fully offline) so `install::verify_bundle_
        //    signature`'s real `codesign --verify` call has something valid
        //    to accept — the same structural shape `install.rs`'s own
        //    fixture-building tests use, but written to real disk and
        //    actually signed rather than a synthetic tar entry. The "binary"
        //    is a COPY of a real system Mach-O (`/bin/echo`), not fake text
        //    bytes — found the hard way (2026-08-22): codesign embeds a real
        //    Mach-O's signature inside its own LC_CODE_SIGNATURE segment
        //    (ordinary file content, survives tar untouched), but for a
        //    non-Mach-O stand-in it falls back to a LEGACY signature
        //    represented as macOS extended attributes on that one file —
        //    which `tar`/`COPYFILE_DISABLE=1` never preserves, so a
        //    plain-text stand-in fails `codesign --verify` after ANY
        //    tar round-trip even though the ORIGINAL (never extracted)
        //    verifies fine. That failure mode is specific to this fixture
        //    methodology; the real `duduclaw-native-gui` binary CI signs is
        //    a genuine Mach-O and is unaffected.
        let staging_root = root.join("staging");
        let new_app = staging_root.join("DuDuClaw.app");
        std::fs::create_dir_all(new_app.join("Contents/MacOS")).unwrap();
        std::fs::write(new_app.join("Contents/Info.plist"), "<plist/>").unwrap();
        std::fs::copy("/bin/echo", new_app.join("Contents/MacOS/duduclaw-native-gui")).unwrap();
        let codesign_status =
            Command::new("codesign").args(["-s", "-", "--force", "--deep"]).arg(&new_app).status().unwrap();
        assert!(codesign_status.success(), "ad-hoc codesign of the fixture bundle failed");

        // 2. tar.gz it, exactly the shape the CI workflow produces
        //    (`tar czf ... -C dist/native-gui-macos DuDuClaw.app`).
        let archive_path = root.join("update.tar.gz");
        // `COPYFILE_DISABLE=1` matches the CI packaging step
        // (`.github/workflows/native-gui-desktop-release.yml`) — WITHOUT it
        // this exact fixture reproduced a real bug this test caught live
        // (see `install::is_apple_double_entry`'s doc comment): macOS `tar`
        // writes a `._DuDuClaw.app` AppleDouble sidecar for the top-level
        // entry because the ad-hoc `codesign` step above adds a
        // `com.apple.provenance` xattr, and that sidecar used to be
        // misidentified as a second, different `.app` bundle. The
        // extractor now filters `._*` entries defensively regardless of
        // this env var — this line stays to match production packaging
        // exactly, not because the extractor still needs it to pass.
        let tar_status = Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .args(["czf"])
            .arg(&archive_path)
            .args(["-C"])
            .arg(&staging_root)
            .arg("DuDuClaw.app")
            .status()
            .unwrap();
        assert!(tar_status.success());
        let archive_bytes = std::fs::read(&archive_path).unwrap();

        // 3. Sign with a FRESH throwaway keypair — never the production key.
        let pub_path = root.join("test.pub");
        let sec_path = root.join("test.key");
        let gen_status = Command::new("minisign")
            .args(["-G", "-W", "-f", "-p"])
            .arg(&pub_path)
            .args(["-s"])
            .arg(&sec_path)
            .status()
            .unwrap();
        assert!(gen_status.success());
        let sig_path = root.join("update.tar.gz.minisig");
        let sign_status = Command::new("minisign")
            .args(["-S", "-s"])
            .arg(&sec_path)
            .args(["-m"])
            .arg(&archive_path)
            .args(["-t", "live e2e fixture", "-x"])
            .arg(&sig_path)
            .status()
            .unwrap();
        assert!(sign_status.success());

        let pub_content = std::fs::read_to_string(&pub_path).unwrap();
        let test_pubkey = pub_content.lines().nth(1).unwrap().trim().to_string();
        let sig_text = std::fs::read_to_string(&sig_path).unwrap();
        let checksum_text = format!("{:x}  update.tar.gz\n", sha2::Sha256::digest(&archive_bytes));

        // ── Negative case: a tampered signature must be refused, and the
        // pipeline must stop there — never reach extraction. ──────────────
        let mut tampered_sig = sig_text.clone();
        // Flip one base64 character in the signature line (line 2) so it
        // decodes but no longer verifies — same "tampered, not just
        // malformed" scenario `verify.rs::tampered_bytes_are_rejected`
        // covers for raw bytes, applied here to the signature itself.
        {
            let lines: Vec<&str> = tampered_sig.lines().collect();
            let mut sig_line: Vec<char> = lines[1].chars().collect();
            let flip_at = sig_line.len() / 2;
            sig_line[flip_at] = if sig_line[flip_at] == 'A' { 'B' } else { 'A' };
            let mut rebuilt = lines.to_vec();
            let flipped: String = sig_line.into_iter().collect();
            let owned = flipped;
            rebuilt[1] = owned.as_str();
            tampered_sig = rebuilt.join("\n") + "\n";
        }
        let reject = verify::verify_minisign(&archive_bytes, &tampered_sig, &test_pubkey);
        assert!(reject.is_err(), "a tampered signature must be rejected");

        // ── Positive case: correct signature verifies, archive extracts,
        // extracted bundle passes its own real codesign check, and swapping
        // it over a stand-in "current app" directory actually replaces it
        // on disk. ──────────────────────────────────────────────────────
        verify::verify_sha256(&archive_bytes, &checksum_text).expect("checksum must match");
        verify::verify_minisign(&archive_bytes, &sig_text, &test_pubkey).expect("correct signature must verify");

        let current_app = root.join("installed").join("DuDuClaw.app");
        std::fs::create_dir_all(&current_app).unwrap();
        std::fs::write(current_app.join("marker.txt"), "old version").unwrap();

        let extract_dest = install::stage_dir_for(&current_app, "live-e2e").unwrap();
        let extracted = install::extract_app_bundle(&archive_bytes, &extract_dest).unwrap();
        install::verify_bundle_signature(&extracted).expect("ad-hoc-signed fixture must pass real codesign --verify");
        install::swap_app_bundle(&current_app, &extracted).unwrap();

        assert!(current_app.join("Contents/MacOS/duduclaw-native-gui").exists());
        assert!(!current_app.join("marker.txt").exists(), "old bundle content must be gone after the swap");
        let mut backup_os = current_app.as_os_str().to_owned();
        backup_os.push(".bak");
        assert!(!PathBuf::from(backup_os).exists(), "backup must be cleaned up after a successful swap");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_verified_archive_reports_a_clean_error_off_a_dev_build_layout() {
        // `std::env::current_exe()` in the `cargo test` harness resolves to
        // a `target/.../deps/duduclaw_native_gui-<hash>` test binary, which
        // is never inside a `.app/Contents/MacOS/` layout — this exercises
        // the SAME early-return `running_app_bundle_path` error path a real
        // dev build launched via `cargo run` would hit, end to end through
        // `install_verified_archive`, without needing a real `.app` on disk
        // or a real signed archive.
        let err = install_verified_archive(b"irrelevant, never reached").unwrap_err();
        assert!(!err.is_empty());
    }
}
