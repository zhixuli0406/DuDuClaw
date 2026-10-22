//! Pinned ONNX Runtime release archives.
//!
//! Every byte pulled off the network is named here with its size and
//! SHA-256. There is no "latest" resolution at runtime: a version bump is a
//! code change that goes through review.
//!
//! The version matches what `ort-sys 2.0.0-rc.12` targets (1.24.2). We
//! extract exactly one member (the versioned shared library) out of each
//! Microsoft release archive.
//!
//! `ort-archives.json` next to this crate's `Cargo.toml` carries the same
//! table for the offline-bundle script; a test keeps the two identical.

use std::path::{Path, PathBuf};

/// ONNX Runtime version. Must match what `ort-sys 2.0.0-rc.12` targets —
/// its own crate description says "ONNX Runtime 1.24" and its bundled
/// `dist.txt` pins `ms@1.24.2`. Loading a mismatched major/minor through
/// `load-dynamic` is how you get an ABI crash instead of an error.
pub const ORT_VERSION: &str = "1.24.2";

/// Archive container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// gzip-compressed tar (`.tgz`) — the macOS / Linux releases.
    TarGz,
    /// zip (`.zip`) — the Windows releases.
    Zip,
}

impl ArchiveKind {
    /// File extension of a downloaded archive.
    pub fn extension(self) -> &'static str {
        match self {
            ArchiveKind::TarGz => "tgz",
            ArchiveKind::Zip => "zip",
        }
    }
}

/// A pinned ONNX Runtime release archive plus the one member we keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrtArchive {
    /// Rust target triple this archive serves.
    pub target: &'static str,
    /// Download URL (Microsoft GitHub release asset).
    pub url: &'static str,
    /// Archive size in bytes.
    pub size: u64,
    /// Archive SHA-256, lowercase hex.
    pub sha256: &'static str,
    pub kind: ArchiveKind,
    /// Path of the shared library INSIDE the archive. We extract only this
    /// one member — the archives also carry headers, cmake files, debug
    /// symbols and test data we have no use for, and (on macOS/Linux) an
    /// unversioned symlink we would rather create as a plain file.
    pub member: &'static str,
    /// File name written under `<home>/lib/onnxruntime/<ver>/`. This is what
    /// `ort::init_from` is pointed at.
    pub lib_file: &'static str,
}

/// Every platform we can install ONNX Runtime for.
///
/// **macOS on Intel is absent on purpose.** Microsoft stopped shipping an
/// `onnxruntime-osx-x86_64` asset before 1.24, and `ort`'s own binary
/// distribution has no `x86_64-apple-darwin` entry at 1.24.2 either. Rather
/// than substitute an unrelated build, that platform reports "unsupported"
/// and every feature that needs a model fails closed.
pub const ORT_ARCHIVES: &[OrtArchive] = &[
    OrtArchive {
        target: "aarch64-apple-darwin",
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-osx-arm64-1.24.2.tgz",
        size: 31_604_221,
        sha256: "0af4fa503e8ea285245b47ee42d0a7461b8156a81270857da0c1d4ecf858abde",
        kind: ArchiveKind::TarGz,
        member: "onnxruntime-osx-arm64-1.24.2/lib/libonnxruntime.1.24.2.dylib",
        lib_file: "libonnxruntime.dylib",
    },
    OrtArchive {
        target: "x86_64-unknown-linux-gnu",
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-x64-1.24.2.tgz",
        size: 8_123_282,
        sha256: "43725474ba5663642e17684717946693850e2005efbd724ac72da278fead25e6",
        kind: ArchiveKind::TarGz,
        member: "onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so.1.24.2",
        lib_file: "libonnxruntime.so",
    },
    OrtArchive {
        target: "aarch64-unknown-linux-gnu",
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-aarch64-1.24.2.tgz",
        size: 7_135_756,
        sha256: "6715b3d19965a2a6981e78ed4ba24f17a8c30d2d26420dbed10aac7ceca0085e",
        kind: ArchiveKind::TarGz,
        member: "onnxruntime-linux-aarch64-1.24.2/lib/libonnxruntime.so.1.24.2",
        lib_file: "libonnxruntime.so",
    },
    OrtArchive {
        target: "x86_64-pc-windows-msvc",
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-win-x64-1.24.2.zip",
        size: 74_075_355,
        sha256: "8e3e9c826375352e29cb2614fe44f3d7a4b0ff7b8028ad7a456af9d949a7e8b0",
        kind: ArchiveKind::Zip,
        member: "onnxruntime-win-x64-1.24.2/lib/onnxruntime.dll",
        lib_file: "onnxruntime.dll",
    },
    OrtArchive {
        target: "aarch64-pc-windows-msvc",
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-win-arm64-1.24.2.zip",
        size: 74_960_464,
        sha256: "dd8180d98e5a0ead7ead99029acc80b86a8b905b9aba4cc978e388039bb5823b",
        kind: ArchiveKind::Zip,
        member: "onnxruntime-win-arm64-1.24.2/lib/onnxruntime.dll",
        lib_file: "onnxruntime.dll",
    },
];

/// The Rust target triple this binary was built for, as the manifest spells
/// it. Windows MSVC and GNU (mingw) toolchains consume the same DLL, so both
/// map onto the `-msvc` entry.
pub fn current_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin" // deliberately not in ORT_ARCHIVES — see above
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "aarch64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}

/// The archive for `target`, if one is pinned.
pub fn ort_archive_for_target(target: &str) -> Option<&'static OrtArchive> {
    ORT_ARCHIVES.iter().find(|a| a.target == target)
}

/// The archive for the running platform, or `None` when this platform has no
/// pinned ONNX Runtime build.
pub fn ort_archive_for_current_platform() -> Option<&'static OrtArchive> {
    ort_archive_for_target(current_target())
}

/// `<home>/lib/onnxruntime` — the shared ORT root under a DuDuClaw home.
pub fn ort_lib_root_under_home(home: &Path) -> PathBuf {
    home.join("lib").join("onnxruntime")
}

/// `<ort_lib_root>/<version>/` — where the shared library is installed.
pub fn ort_lib_dir(ort_lib_root: &Path) -> PathBuf {
    ort_lib_root.join(ORT_VERSION)
}

/// Full path of the shared library for the running platform, or `None` on an
/// unsupported platform.
pub fn ort_lib_path(ort_lib_root: &Path) -> Option<PathBuf> {
    ort_archive_for_current_platform().map(|a| ort_lib_dir(ort_lib_root).join(a.lib_file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ort_archives_are_pinned_and_https() {
        assert_eq!(ORT_ARCHIVES.len(), 5);
        for a in ORT_ARCHIVES {
            assert!(
                a.url.starts_with(
                    "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/"
                ),
                "{} url must be an official Microsoft release asset: {}",
                a.target,
                a.url
            );
            assert!(a.url.contains(ORT_VERSION));
            assert_eq!(a.sha256.len(), 64);
            assert!(
                a.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
            assert!(a.member.contains(ORT_VERSION) || a.member.ends_with(".dll"));
            assert!(a.size > 1_000_000);
        }
    }

    #[test]
    fn macos_intel_is_explicitly_unsupported() {
        assert!(ort_archive_for_target("x86_64-apple-darwin").is_none());
    }

    #[test]
    fn lib_path_is_versioned_under_the_shared_root() {
        let root = ort_lib_root_under_home(Path::new("/h"));
        assert_eq!(root, PathBuf::from("/h").join("lib").join("onnxruntime"));
        if let Some(p) = ort_lib_path(&root) {
            assert!(p.to_string_lossy().contains(ORT_VERSION), "{p:?}");
        }
    }

    /// `ort-archives.json` (read by `scripts/kb-model-bundle.sh`) must say
    /// exactly what the Rust table says.
    #[test]
    fn json_manifest_matches_the_rust_table() {
        let raw = include_str!("../ort-archives.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(v["ort_version"], ORT_VERSION);
        let list = v["archives"].as_array().unwrap();
        assert_eq!(list.len(), ORT_ARCHIVES.len());
        for (j, a) in list.iter().zip(ORT_ARCHIVES) {
            assert_eq!(j["target"], a.target);
            assert_eq!(j["url"], a.url);
            assert_eq!(j["size"], a.size);
            assert_eq!(j["sha256"], a.sha256);
            assert_eq!(j["kind"], a.kind.extension());
            assert_eq!(j["member"], a.member);
            assert_eq!(j["lib_file"], a.lib_file);
        }
    }
}
