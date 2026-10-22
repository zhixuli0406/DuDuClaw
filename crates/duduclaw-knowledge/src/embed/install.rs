//! Install, verify and remove a local embedding model.
//!
//! Online: every pinned file is streamed from Hugging Face at the pinned
//! commit (resumable, SHA-256 checked, renamed into place), the shared ONNX
//! Runtime library is installed under `<home>/lib/onnxruntime/<ver>/` when
//! missing, then [`INSTALL_MARKER`] is written. Offline: the operator unpacks
//! a bundle into the home and [`verify_offline`] hashes every file (and the
//! bundled ORT archive) before writing the same marker.
//!
//! The marker is the only "installed" signal, written last; a partial
//! install reads as not installed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use duduclaw_onnx::install::{self as onnx_install, FileState, InstallLock};
use duduclaw_onnx::manifest as ort_manifest;
use serde::{Deserialize, Serialize};

use super::catalog::{INSTALL_MARKER, ModelSpec};

/// Contents of the install marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub model: String,
    pub revision: String,
    pub ort_version: String,
    pub verified_at: String,
    /// How the files arrived: `download` or `offline`.
    pub via: String,
    pub files: BTreeMap<String, String>,
}

/// Progress of an install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub file: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

/// Progress sink; cheap, never blocks.
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Where the shared ONNX Runtime lives under `home`.
pub fn ort_root(home: &Path) -> PathBuf {
    ort_manifest::ort_lib_root_under_home(home)
}

/// The ORT library path for this platform, `None` when unsupported.
pub fn ort_lib(home: &Path) -> Option<PathBuf> {
    ort_manifest::ort_lib_path(&ort_root(home))
}

/// Read the marker of `spec` under `home`.
pub fn read_marker(spec: &ModelSpec, home: &Path) -> Option<Marker> {
    let raw = std::fs::read_to_string(spec.install_dir(home).join(INSTALL_MARKER)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Why `spec` is not usable on this machine, or `None` when it is. Cheap:
/// marker + sizes + the ORT library's presence; hashing happened at install.
pub fn installed_problem(spec: &ModelSpec, home: &Path) -> Option<String> {
    if ort_manifest::ort_archive_for_current_platform().is_none() {
        return Some(format!(
            "這個平台（{}）沒有可用的 ONNX Runtime，無法使用本機向量模型",
            ort_manifest::current_target()
        ));
    }
    let Some(marker) = read_marker(spec, home) else {
        return Some("尚未安裝向量模型".to_string());
    };
    if marker.revision != spec.revision || marker.model != spec.id {
        return Some("已安裝的向量模型版本與這個 DuDuClaw 不符，請重新安裝".to_string());
    }
    let dir = spec.install_dir(home);
    for f in &spec.files {
        match std::fs::metadata(ModelSpec::dest(&dir, f)) {
            Ok(m) if m.len() == f.size => {}
            Ok(_) => return Some(format!("向量模型檔 {} 大小不符，請重新安裝", f.path)),
            Err(_) => return Some(format!("向量模型檔 {} 不存在，請重新安裝", f.path)),
        }
    }
    // Checked against its pin before anything may load it.
    if let Err(problem) = onnx_install::verify_ort_lib(&ort_root(home)) {
        return Some(problem);
    }
    None
}

/// Bytes an online install moves on this platform (model + ORT if missing).
pub fn total_install_bytes(spec: &ModelSpec, home: &Path) -> u64 {
    let ort = match ort_manifest::ort_archive_for_current_platform() {
        Some(a) if !onnx_install::ort_installed(&ort_root(home)) => a.size,
        _ => 0,
    };
    spec.total_bytes() + ort
}

fn write_marker(spec: &ModelSpec, home: &Path, via: &str) -> Result<(), String> {
    let marker = Marker {
        model: spec.id.clone(),
        revision: spec.revision.clone(),
        ort_version: ort_manifest::ORT_VERSION.to_string(),
        verified_at: crate::now_rfc3339(),
        via: via.to_string(),
        files: spec
            .files
            .iter()
            .map(|f| (f.path.clone(), f.sha256.clone()))
            .collect(),
    };
    let body = serde_json::to_string_pretty(&marker).map_err(|e| e.to_string())?;
    let path = spec.install_dir(home).join(INSTALL_MARKER);
    duduclaw_core::with_file_lock(&path, || {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body.as_bytes())?;
        std::fs::rename(&tmp, &path)
    })
    .map_err(|e| format!("無法寫入安裝紀錄：{e}"))
}

/// Download and install `spec` (and ORT when missing). Idempotent: a file
/// already at its pinned size is kept; a partial one resumes. `cancel`
/// stops between chunks and keeps the partial files for the next attempt.
pub async fn install(
    spec: &ModelSpec,
    home: &Path,
    progress: ProgressFn,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let Some(archive) = ort_manifest::ort_archive_for_current_platform() else {
        return Err(format!(
            "這個平台（{}）沒有可用的 ONNX Runtime，無法安裝本機向量模型",
            ort_manifest::current_target()
        ));
    };
    let dir = spec.install_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("無法建立模型資料夾：{e}"))?;
    let _lock = InstallLock::acquire(&dir).map_err(|e| e.to_string())?;
    let total = total_install_bytes(spec, home);
    let client =
        onnx_install::http_client(concat!("duduclaw-knowledge/", env!("CARGO_PKG_VERSION")))
            .map_err(|e| e.to_string())?;
    let mut done = 0u64;
    for f in &spec.files {
        let dest = ModelSpec::dest(&dir, f);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if std::fs::metadata(&dest).map(|m| m.len()).ok() == Some(f.size) {
            done += f.size;
            progress(Progress {
                file: f.path.clone(),
                done_bytes: done,
                total_bytes: total,
            });
            continue;
        }
        let base = done;
        let label = f.path.clone();
        let p = progress.clone();
        onnx_install::fetch_verified(
            &client,
            &spec.url(f),
            &dest,
            f.size,
            &f.sha256,
            &cancel,
            &move |got| {
                p(Progress {
                    file: label.clone(),
                    done_bytes: base + got,
                    total_bytes: total,
                });
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        done += f.size;
    }
    let ort_root = ort_root(home);
    if !onnx_install::ort_installed(&ort_root) {
        let base = done;
        let p = progress.clone();
        onnx_install::install_ort(&client, &ort_root, archive, &cancel, &move |got| {
            p(Progress {
                file: "onnxruntime".into(),
                done_bytes: base + got,
                total_bytes: total,
            });
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    write_marker(spec, home, "download")?;
    progress(Progress {
        file: "done".into(),
        done_bytes: total,
        total_bytes: total,
    });
    Ok(())
}

/// Offline install from a signed bundle the operator copied into
/// `<home>/models/embedding/` (`duduclaw-kb-model-<model>-<target>.tar` and
/// its `.tar.minisig`). Order: signature over the whole tar against
/// `release_pubkey` (its trusted comment must name the file) → unpack only
/// the expected regular files → every model file against its pin → the
/// bundled ORT archive against its pin, extracted and the library checked
/// against its own pin → marker. The bundle files are deleted on success.
pub fn verify_offline(spec: &ModelSpec, home: &Path, release_pubkey: &str) -> Result<(), String> {
    let Some(archive) = ort_manifest::ort_archive_for_current_platform() else {
        return Err(format!(
            "這個平台（{}）沒有可用的 ONNX Runtime，無法使用本機向量模型",
            ort_manifest::current_target()
        ));
    };
    let name = super::bundle::bundle_name(spec, ort_manifest::current_target());
    let Some((tar, sig)) = super::bundle::find(spec, home) else {
        return Err(format!(
            "找不到離線安裝包：請把 {name} 與 {name}.minisig 一起放進 DuDuClaw 資料夾的 models/embedding"
        ));
    };
    super::bundle::verify_signature(&tar, &sig, release_pubkey, &name)?;
    super::bundle::unpack(&tar, spec, home)?;
    let dir = spec.install_dir(home);
    for f in &spec.files {
        match onnx_install::verify_path(&ModelSpec::dest(&dir, f), &f.path, f.size, &f.sha256) {
            FileState::Verified => {}
            FileState::Missing => return Err(format!("找不到模型檔 {}", f.path)),
            FileState::Corrupt(why) => return Err(format!("模型檔驗證失敗：{why}")),
        }
    }
    onnx_install::install_ort_from_local_archive(&ort_root(home), archive)
        .map_err(|e| e.to_string())?;
    write_marker(spec, home, "offline")?;
    let _ = std::fs::remove_file(&tar);
    let _ = std::fs::remove_file(&sig);
    Ok(())
}

/// Delete the model files and the marker (marker first, so a failure
/// midway reads as "not installed"). The shared ORT library stays — the
/// redaction model may use it.
pub fn remove(spec: &ModelSpec, home: &Path) -> Result<(), String> {
    let dir = spec.install_dir(home);
    let marker = dir.join(INSTALL_MARKER);
    if marker.exists() {
        std::fs::remove_file(&marker).map_err(|e| e.to_string())?;
    }
    // The advisory-lock sentinel `with_file_lock` leaves next to the marker.
    let _ = std::fs::remove_file(dir.join(format!("{INSTALL_MARKER}.lock")));
    // The manifest an offline bundle carries (scripts/kb-model-bundle.sh).
    let _ = std::fs::remove_file(dir.join("bundle.json"));
    for f in &spec.files {
        let p = ModelSpec::dest(&dir, f);
        for q in [onnx_install::part_path(&p), p] {
            if q.exists() {
                std::fs::remove_file(&q).map_err(|e| e.to_string())?;
            }
        }
    }
    let _ = std::fs::remove_dir(dir.join("onnx"));
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::catalog::{ModelFile, Pooling};
    use sha2::{Digest, Sha256};

    fn tiny_spec(files: &[(&str, &[u8])]) -> ModelSpec {
        ModelSpec {
            id: "tiny".into(),
            display_name: "tiny".into(),
            repo: "x/y".into(),
            revision: "a".repeat(40),
            license: "MIT".into(),
            license_source: String::new(),
            dim: 2,
            native_dim: 2,
            max_tokens: 8,
            pooling: Pooling::Mean,
            query_prefix: String::new(),
            passage_prefix: String::new(),
            model_file: files[0].0.into(),
            tokenizer_file: files[0].0.into(),
            kv_heads: 0,
            head_dim: 0,
            speed: "fast".into(),
            batch_limit: 8,
            files: files
                .iter()
                .map(|(p, b)| ModelFile {
                    path: p.to_string(),
                    size: b.len() as u64,
                    sha256: hex::encode(Sha256::digest(b)),
                })
                .collect(),
        }
    }

    /// Put a signed bundle of `files` for `spec` into `home`; returns the
    /// public key.
    fn place_bundle(
        spec: &ModelSpec,
        home: &Path,
        files: &[(&str, &[u8])],
        comment_name: &str,
    ) -> String {
        use crate::embed::bundle::{bundle_dir, bundle_name, test_support};
        std::fs::create_dir_all(bundle_dir(home)).unwrap();
        let name = bundle_name(spec, ort_manifest::current_target());
        let tar = bundle_dir(home).join(&name);
        let entries: Vec<(String, &[u8])> = files
            .iter()
            .map(|(p, b)| (format!("models/embedding/{}/{p}", spec.id), *b))
            .collect();
        let refs: Vec<(&str, &[u8])> = entries.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        test_support::tar_with(&refs, &tar);
        let (pk, sig) = test_support::sign(
            &std::fs::read(&tar).unwrap(),
            &format!("duduclaw-kb-model test {comment_name}"),
        );
        std::fs::write(bundle_dir(home).join(format!("{name}.minisig")), sig).unwrap();
        pk
    }

    #[test]
    fn offline_install_checks_signature_then_pins_and_writes_the_marker_last() {
        if ort_manifest::ort_archive_for_current_platform().is_none() {
            return; // unsupported platform: covered by the next test
        }
        let home = tempfile::tempdir().unwrap();
        let spec = tiny_spec(&[("onnx/m.onnx", b"model"), ("tok.json", b"{}")]);
        let name = crate::embed::bundle::bundle_name(&spec, ort_manifest::current_target());
        // No bundle at all.
        let (other_pk, _) = crate::embed::bundle::test_support::sign(b"x", "x");
        let err = verify_offline(&spec, home.path(), &other_pk).unwrap_err();
        assert!(err.contains(".minisig"), "{err}");
        // A bundle signed by another key: nothing is unpacked.
        place_bundle(
            &spec,
            home.path(),
            &[("onnx/m.onnx", b"model"), ("tok.json", b"{}")],
            &name,
        );
        let err = verify_offline(&spec, home.path(), &other_pk).unwrap_err();
        assert!(err.contains("簽章"), "{err}");
        assert!(!spec.install_dir(home.path()).join("onnx/m.onnx").exists());
        // Signed for a different file name: refused.
        let pk = place_bundle(
            &spec,
            home.path(),
            &[("onnx/m.onnx", b"model"), ("tok.json", b"{}")],
            "other.tar",
        );
        assert!(
            verify_offline(&spec, home.path(), &pk)
                .unwrap_err()
                .contains("檔名")
        );
        // Correctly signed, but a file that does not match its pin.
        let pk = place_bundle(
            &spec,
            home.path(),
            &[("onnx/m.onnx", b"model"), ("tok.json", b"{ }")],
            &name,
        );
        assert!(
            verify_offline(&spec, home.path(), &pk)
                .unwrap_err()
                .contains("驗證失敗")
        );
        assert!(read_marker(&spec, home.path()).is_none());
        // Correct model files; the bundle has no runtime archive and no
        // verified library exists, so the install still stops.
        let pk = place_bundle(
            &spec,
            home.path(),
            &[("onnx/m.onnx", b"model"), ("tok.json", b"{}")],
            &name,
        );
        let err = verify_offline(&spec, home.path(), &pk).unwrap_err();
        assert!(err.contains("ONNX Runtime"), "{err}");
        assert!(read_marker(&spec, home.path()).is_none());
        // A library dropped in by hand is reported, never trusted.
        let lib_dir = ort_manifest::ort_lib_dir(&ort_root(home.path()));
        std::fs::create_dir_all(&lib_dir).unwrap();
        std::fs::write(ort_lib(home.path()).unwrap(), b"lib").unwrap();
        write_marker(&spec, home.path(), "offline").unwrap();
        let problem = installed_problem(&spec, home.path()).unwrap();
        assert!(problem.contains("不符"), "{problem}");
        remove(&spec, home.path()).unwrap();
        assert_eq!(
            installed_problem(&spec, home.path()).as_deref(),
            Some("尚未安裝向量模型")
        );
    }

    #[test]
    fn a_marker_for_another_revision_is_not_installed() {
        let home = tempfile::tempdir().unwrap();
        let spec = tiny_spec(&[("m.onnx", b"model")]);
        let dir = spec.install_dir(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.onnx"), b"model").unwrap();
        let mut other = spec.clone();
        other.revision = "b".repeat(40);
        write_marker(&other, home.path(), "download").unwrap();
        let problem = installed_problem(&spec, home.path()).unwrap();
        if ort_manifest::ort_archive_for_current_platform().is_some() {
            assert!(problem.contains("版本"), "{problem}");
        } else {
            assert!(problem.contains("平台"), "{problem}");
        }
    }
}
