//! Cross-platform primary-monitor screen capture by shelling out to the OS's
//! native screenshot tool. Returns PNG-encoded bytes.
//!
//! Replaces the `xcap` crate, whose Linux backends (`xcb`, `wayland-scanner`)
//! pull a build-time `quick-xml < 0.41` flagged by RUSTSEC-2026-0194/0195. The
//! native tools emit PNG directly, so no in-process image re-encode is needed.
//!
//! Tool per platform:
//! - macOS: `screencapture` (always present)
//! - Windows: PowerShell + `System.Drawing` (always present)
//! - Linux: the first available of `grim` (Wayland), `scrot`, `maim`,
//!   `import` (ImageMagick) — install one for native L5b capture

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SHOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Capture the primary monitor and return PNG-encoded bytes.
///
/// Returns `Err(msg)` when no capture tool is available or the tool fails.
/// Writes to a unique temp file, reads it back, and removes it.
pub fn capture_primary_monitor_png() -> Result<Vec<u8>, String> {
    let out = temp_png_path();
    let captured = capture_to(&out);
    let bytes = captured.and_then(|()| std::fs::read(&out).map_err(|e| format!("read screenshot: {e}")));
    let _ = std::fs::remove_file(&out); // best-effort cleanup regardless of outcome
    let bytes = bytes?;
    if bytes.is_empty() {
        return Err("screenshot tool produced an empty file".into());
    }
    Ok(bytes)
}

fn temp_png_path() -> PathBuf {
    let n = SHOT_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("duduclaw-shot-{pid}-{n}.png"))
}

/// Run `bin args...`, discarding stdout and surfacing stderr on failure.
fn run(bin: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let tail = String::from_utf8_lossy(&output.stderr);
        Err(format!("{bin} exited with {}: {}", output.status, tail.trim()))
    }
}

#[cfg(target_os = "macos")]
fn capture_to(out: &Path) -> Result<(), String> {
    // `-x` silences the capture sound; no `-D` → the main display.
    run("screencapture", &["-x", "-t", "png", &out.to_string_lossy()])
}

#[cfg(target_os = "windows")]
fn capture_to(out: &Path) -> Result<(), String> {
    // Copy the primary screen bounds into a PNG via System.Drawing.
    let path = out.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
         $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds; \
         $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height; \
         $g = [System.Drawing.Graphics]::FromImage($bmp); \
         $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size); \
         $bmp.Save('{path}', [System.Drawing.Imaging.ImageFormat]::Png); \
         $g.Dispose(); $bmp.Dispose()"
    );
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script.as_str()],
    )
}

#[cfg(target_os = "linux")]
fn capture_to(out: &Path) -> Result<(), String> {
    let path = out.to_string_lossy().to_string();
    // First available tool wins: Wayland (`grim`) then X11 (`scrot`/`maim`/`import`).
    let candidates: [(&str, Vec<String>); 4] = [
        ("grim", vec![path.clone()]),
        ("scrot", vec!["-o".into(), path.clone()]),
        ("maim", vec![path.clone()]),
        ("import", vec!["-window".into(), "root".into(), path.clone()]),
    ];
    let mut last_err =
        "no supported screenshot tool found (install grim, scrot, maim, or ImageMagick)".to_string();
    for (bin, args) in &candidates {
        if !on_path(bin) {
            continue;
        }
        let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
        match run(bin, &argrefs) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn capture_to(_out: &Path) -> Result<(), String> {
    Err("native screenshot is unsupported on this platform".into())
}

/// Dependency-free PATH probe for a binary (Linux tool selection).
#[cfg(target_os = "linux")]
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_paths_are_unique_and_png() {
        let a = temp_png_path();
        let b = temp_png_path();
        assert_ne!(a, b, "sequential temp paths must differ");
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("duduclaw-shot-"));
    }

    #[test]
    fn run_reports_spawn_failure() {
        let err = run("duduclaw-nonexistent-binary-xyz", &[]).unwrap_err();
        assert!(err.contains("spawn"), "expected a spawn error, got: {err}");
    }
}
