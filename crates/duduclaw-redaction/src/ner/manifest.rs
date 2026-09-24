//! Pinned download manifest for the NER model and the ONNX Runtime shared
//! library.
//!
//! Every byte this feature pulls off the network is named here with its size
//! and SHA-256. There is no "latest" resolution at runtime: a revision bump
//! is a code change that goes through review, because the alternative is a
//! redaction engine whose behaviour can change under an operator without a
//! deploy.
//!
//! Two independent artefacts:
//!
//! * **Model** — `openai/privacy-filter` on Hugging Face, pinned to one
//!   commit. Apache-2.0. Six files, ~945 MB, dominated by
//!   `onnx/model_q4.onnx_data`.
//! * **ONNX Runtime** — Microsoft's official GitHub release matching the
//!   version `ort-sys 2.0.0-rc.12` targets (1.24.2). We extract exactly one
//!   member (the versioned shared library) out of each archive.

use std::path::{Path, PathBuf};

/// Hugging Face commit the model files are pinned to.
///
/// `openai/privacy-filter` @ 2026-04-22. Shown to operators as the installed
/// model version and recorded in every `AuditEvent::Redact` the model
/// produces.
pub const MODEL_REVISION: &str = "7ffa9a043d54d1be65afb281eddf0ffbe629385b";

/// ONNX Runtime version. Must match what `ort-sys 2.0.0-rc.12` targets —
/// its own crate description says "ONNX Runtime 1.24" and its bundled
/// `dist.txt` pins `ms@1.24.2`. Loading a mismatched major/minor through
/// `load-dynamic` is how you get an ABI crash instead of an error.
pub const ORT_VERSION: &str = "1.24.2";

/// Total model download size, for the UI's "917 MB" style warning.
pub fn model_total_bytes() -> u64 {
    MODEL_FILES.iter().map(|f| f.size).sum()
}

/// One pinned remote file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteFile {
    /// Path relative to the model directory (also the path inside the HF repo).
    pub rel_path: &'static str,
    /// Expected byte length.
    pub size: u64,
    /// Expected SHA-256, lowercase hex.
    pub sha256: &'static str,
}

impl RemoteFile {
    /// Absolute download URL for this file at [`MODEL_REVISION`].
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/openai/privacy-filter/resolve/{MODEL_REVISION}/{}",
            self.rel_path
        )
    }

    /// Where this file lands under `model_dir`.
    pub fn dest(&self, model_dir: &Path) -> PathBuf {
        let mut p = model_dir.to_path_buf();
        for seg in self.rel_path.split('/') {
            p.push(seg);
        }
        p
    }
}

/// The six files a working install needs.
///
/// Hashes for the four LFS-backed files were cross-checked against Hugging
/// Face's own `paths-info` LFS oids at [`MODEL_REVISION`]; the three small
/// plain-git files are hashed from the same revision's checkout.
pub const MODEL_FILES: &[RemoteFile] = &[
    RemoteFile {
        rel_path: "config.json",
        size: 3039,
        sha256: "b2b26a4a4a000639ad30b0c264adbefe365bdb567fbd7bb27303b8c438375bd1",
    },
    RemoteFile {
        rel_path: "tokenizer.json",
        size: 27_868_174,
        sha256: "0614fe83cadab421296e664e1f48f4261fa8fef6e03e63bb75c20f38e37d07d3",
    },
    RemoteFile {
        rel_path: "tokenizer_config.json",
        size: 234,
        sha256: "6c14af9ce1a284d3c3c5146b26efe4cd589c68e1dd4e9d94455606ec911ba774",
    },
    RemoteFile {
        rel_path: "viterbi_calibration.json",
        size: 372,
        sha256: "bbc8611ef08a55ed72d64856cbbbb9a91db8dfa881f0a92e2afbad6e4bbc775a",
    },
    RemoteFile {
        rel_path: "onnx/model_q4.onnx",
        size: 160_219,
        sha256: "8f7dee8b46d096f052b359375dfba5d983cc4d18c44a783bf548615c472f8dea",
    },
    RemoteFile {
        rel_path: "onnx/model_q4.onnx_data",
        size: 917_120_144,
        sha256: "f30998e28c71c5374cc7e8b7de8f0f83e981592c0c2d652d2ad4928454dbb496",
    },
];

/// Archive container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// gzip-compressed tar (`.tgz`) — the macOS / Linux releases.
    TarGz,
    /// zip (`.zip`) — the Windows releases.
    Zip,
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
/// and the rule fails closed.
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

/// The archive for the running platform, or `None` when this platform has no
/// pinned ONNX Runtime build.
pub fn ort_archive_for_current_platform() -> Option<&'static OrtArchive> {
    let target = current_target();
    ORT_ARCHIVES.iter().find(|a| a.target == target)
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

/// Marker file written after a fully verified install.
pub const INSTALL_MARKER: &str = "installed.json";

/// Contents of [`INSTALL_MARKER`] — what was verified, and when.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstallMarker {
    pub model_revision: String,
    pub ort_version: String,
    /// RFC-3339 timestamp of the verification that produced this marker.
    pub verified_at: String,
    /// `rel_path` → sha256 actually computed during install. Recorded so a
    /// future audit can tell which bytes were accepted without re-hashing a
    /// gigabyte.
    pub files: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_files_are_pinned_with_plausible_hashes() {
        assert_eq!(MODEL_FILES.len(), 6);
        for f in MODEL_FILES {
            assert_eq!(f.sha256.len(), 64, "{} sha must be hex-64", f.rel_path);
            assert!(
                f.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} sha must be lowercase hex",
                f.rel_path
            );
            assert!(f.size > 0);
        }
    }

    #[test]
    fn model_urls_carry_the_pinned_revision() {
        let u = MODEL_FILES[0].url();
        assert!(u.contains(MODEL_REVISION), "{u}");
        assert!(u.starts_with("https://huggingface.co/openai/privacy-filter/"), "{u}");
        // Never `main` — a moving ref would let the model change under an
        // operator with no deploy.
        assert!(!u.contains("/resolve/main/"), "{u}");
    }

    #[test]
    fn model_total_is_about_945_mb() {
        let total = model_total_bytes();
        assert!(total > 900_000_000 && total < 1_000_000_000, "{total}");
    }

    #[test]
    fn dest_nests_the_onnx_subdirectory() {
        let f = MODEL_FILES.iter().find(|f| f.rel_path == "onnx/model_q4.onnx").unwrap();
        let dest = f.dest(Path::new("/m"));
        assert_eq!(dest, PathBuf::from("/m").join("onnx").join("model_q4.onnx"));
    }

    #[test]
    fn ort_archives_are_pinned_and_https() {
        assert_eq!(ORT_ARCHIVES.len(), 5);
        for a in ORT_ARCHIVES {
            assert!(
                a.url.starts_with("https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/"),
                "{} url must be an official Microsoft release asset: {}",
                a.target,
                a.url
            );
            assert!(a.url.contains(ORT_VERSION), "{} url must carry the pinned version", a.target);
            assert_eq!(a.sha256.len(), 64);
            assert!(a.member.contains(ORT_VERSION) || a.member.ends_with(".dll"));
            assert!(a.size > 1_000_000);
        }
    }

    #[test]
    fn macos_intel_is_explicitly_unsupported() {
        // Microsoft publishes no osx-x86_64 asset at 1.24.x; pretending
        // otherwise would mean shipping a library that cannot load.
        assert!(
            !ORT_ARCHIVES.iter().any(|a| a.target == "x86_64-apple-darwin"),
            "an osx-x86_64 entry must be a deliberate decision, not a copy-paste"
        );
    }

    #[test]
    fn lib_path_is_versioned() {
        if let Some(p) = ort_lib_path(Path::new("/lib")) {
            assert!(p.to_string_lossy().contains(ORT_VERSION), "{p:?}");
        }
    }
}
