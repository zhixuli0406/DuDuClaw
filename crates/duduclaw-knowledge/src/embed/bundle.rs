//! Signed offline bundles (`duduclaw-kb-model-<model>-<target>.tar` +
//! `.tar.minisig`).
//!
//! The operator copies both files into `<home>/models/embedding/`. Before a
//! single byte is unpacked the minisign signature is checked against the
//! release public key the caller passes in (the same key the self-updater
//! trusts) and its trusted comment must name this exact bundle file. Only
//! then is the tar read, and only the entries the catalog expects are
//! written (regular files at fixed paths; links, absolute paths, `..` or
//! unexpected files refuse the whole bundle). The per-file SHA-256 pins are
//! checked again after unpacking by [`super::install::verify_offline`].

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use duduclaw_onnx::manifest as ort_manifest;

use super::catalog::ModelSpec;

/// `duduclaw-kb-model-<id>-<target>.tar`.
pub fn bundle_name(spec: &ModelSpec, target: &str) -> String {
    format!("duduclaw-kb-model-{}-{target}.tar", spec.id)
}

/// Where the operator puts bundles: `<home>/models/embedding/`.
pub fn bundle_dir(home: &Path) -> PathBuf {
    home.join("models").join("embedding")
}

/// The bundle and signature for `spec` on this platform, if both are there.
pub fn find(spec: &ModelSpec, home: &Path) -> Option<(PathBuf, PathBuf)> {
    let tar = bundle_dir(home).join(bundle_name(spec, ort_manifest::current_target()));
    let mut sig = tar.clone().into_os_string();
    sig.push(".minisig");
    let sig = PathBuf::from(sig);
    (tar.is_file() && sig.is_file()).then_some((tar, sig))
}

/// Check `sig_path` over the whole of `tar` (streamed) with `pubkey_b64`,
/// and that the trusted comment names `expected_name` as one of its words.
pub fn verify_signature(
    tar: &Path,
    sig_path: &Path,
    pubkey_b64: &str,
    expected_name: &str,
) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("發行公鑰無效：{e}"))?;
    let sig_text = std::fs::read_to_string(sig_path).map_err(|e| format!("無法讀取簽章檔：{e}"))?;
    let sig = minisign_verify::Signature::decode(&sig_text)
        .map_err(|_| "簽章檔格式不正確".to_string())?;
    if !sig
        .trusted_comment()
        .split_whitespace()
        .any(|w| w == expected_name)
    {
        return Err("簽章不是發給這個離線安裝包的（檔名不符）".to_string());
    }
    let mut verifier = pk
        .verify_stream(&sig)
        .map_err(|_| "簽章驗證失敗，未解開安裝包".to_string())?;
    let mut f = std::fs::File::open(tar).map_err(|e| format!("無法讀取安裝包：{e}"))?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("無法讀取安裝包：{e}"))?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier
        .finalize()
        .map_err(|_| "簽章驗證失敗，未解開安裝包".to_string())
}

/// Relative path inside the tar → destination, for everything a bundle of
/// `spec` may contain on this platform.
fn expected_entries(spec: &ModelSpec, home: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let dir = spec.install_dir(home);
    for f in &spec.files {
        map.insert(
            format!("models/embedding/{}/{}", spec.id, f.path),
            ModelSpec::dest(&dir, f),
        );
    }
    if let Some(a) = ort_manifest::ort_archive_for_current_platform() {
        let root = super::install::ort_root(home);
        map.insert(
            format!(
                "lib/onnxruntime/{v}/onnxruntime-{v}.{ext}",
                v = ort_manifest::ORT_VERSION,
                ext = a.kind.extension()
            ),
            duduclaw_onnx::install::ort_archive_path(&root, a),
        );
    }
    map
}

/// Unpack a verified bundle: only the expected regular files, each written
/// to a `.part` file and renamed into place.
pub fn unpack(tar: &Path, spec: &ModelSpec, home: &Path) -> Result<usize, String> {
    let expected = expected_entries(spec, home);
    let manifest = format!("models/embedding/{}/bundle.json", spec.id);
    let f = std::fs::File::open(tar).map_err(|e| format!("無法讀取安裝包：{e}"))?;
    let mut archive = tar::Archive::new(f);
    let mut written = 0usize;
    for entry in archive
        .entries()
        .map_err(|e| format!("安裝包格式不正確：{e}"))?
    {
        let mut entry = entry.map_err(|e| format!("安裝包格式不正確：{e}"))?;
        let kind = entry.header().entry_type();
        let path = entry
            .path()
            .map_err(|_| "安裝包內有無法辨識的路徑".to_string())?
            .to_string_lossy()
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(format!("安裝包內有不允許的項目（連結或特殊檔案）：{path}"));
        }
        if path == manifest {
            continue;
        }
        let Some(dest) = expected.get(&path) else {
            return Err(format!("安裝包內有預期以外的檔案：{path}"));
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let part = duduclaw_onnx::install::part_path(dest);
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("解開安裝包失敗：{e}"))?;
        drop(out);
        std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    /// A tar with `(path, body)` regular files at `at`.
    pub fn tar_with(entries: &[(&str, &[u8])], at: &Path) {
        let f = std::fs::File::create(at).unwrap();
        let mut b = tar::Builder::new(f);
        for (p, body) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, p, *body).unwrap();
        }
        b.finish().unwrap();
    }

    /// `(public key base64, signature text)` over `data` with `comment`.
    pub fn sign(data: &[u8], comment: &str) -> (String, String) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let sig = minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(data),
            Some(comment),
            None,
        )
        .unwrap();
        (kp.pk.to_base64(), sig.into_string())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{sign, tar_with};
    use super::*;

    fn spec() -> ModelSpec {
        let mut s = super::super::catalog::find(super::super::catalog::DEFAULT_MODEL)
            .unwrap()
            .clone();
        s.id = "tiny".into();
        s.files = vec![super::super::catalog::ModelFile {
            path: "onnx/m.onnx".into(),
            size: 5,
            sha256: "0".repeat(64),
        }];
        s
    }

    #[test]
    fn signature_is_checked_before_anything_is_read_and_binds_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let tar = dir.path().join("b.tar");
        tar_with(&[("models/embedding/tiny/onnx/m.onnx", b"model")], &tar);
        let data = std::fs::read(&tar).unwrap();
        let (pk, sig) = sign(&data, "duduclaw-kb-model v1.76.0 b.tar");
        let sig_path = dir.path().join("b.tar.minisig");
        std::fs::write(&sig_path, &sig).unwrap();
        verify_signature(&tar, &sig_path, &pk, "b.tar").unwrap();
        assert!(
            verify_signature(&tar, &sig_path, &pk, "other.tar").is_err(),
            "name bound"
        );
        let (other_pk, _) = sign(b"x", "y");
        assert!(
            verify_signature(&tar, &sig_path, &other_pk, "b.tar").is_err(),
            "wrong key"
        );
        std::fs::write(&tar, [&data[..], b"tampered"].concat()).unwrap();
        assert!(
            verify_signature(&tar, &sig_path, &pk, "b.tar").is_err(),
            "tampered tar"
        );
    }

    #[test]
    fn unpack_writes_only_expected_files() {
        let home = tempfile::tempdir().unwrap();
        let s = spec();
        let tar = home.path().join("ok.tar");
        tar_with(
            &[
                ("models/embedding/tiny/onnx/m.onnx", b"model"),
                ("models/embedding/tiny/bundle.json", b"{}"),
            ],
            &tar,
        );
        assert_eq!(unpack(&tar, &s, home.path()).unwrap(), 1);
        assert_eq!(
            std::fs::read(s.install_dir(home.path()).join("onnx/m.onnx")).unwrap(),
            b"model"
        );

        for bad in [
            "models/embedding/tiny/../../../escape",
            "config.toml",
            "models/embedding/tiny/extra.bin",
        ] {
            let t = home.path().join("bad.tar");
            // `tar::Builder` refuses `..` itself, so write the raw header.
            let f = std::fs::File::create(&t).unwrap();
            let mut b = tar::Builder::new(f);
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            {
                let name = h.as_old_mut().name.as_mut();
                name[..bad.len()].copy_from_slice(bad.as_bytes());
            }
            h.set_cksum();
            b.append(&h, &b"x"[..]).unwrap();
            b.finish().unwrap();
            assert!(unpack(&t, &s, home.path()).is_err(), "{bad}");
        }
        assert!(!home.path().join("escape").exists());
        assert!(!home.path().join("config.toml").exists());
    }

    #[test]
    fn links_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let t = home.path().join("link.tar");
        let f = std::fs::File::create(&t).unwrap();
        let mut b = tar::Builder::new(f);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "models/embedding/tiny/onnx/m.onnx", "/etc/passwd")
            .unwrap();
        b.finish().unwrap();
        assert!(
            unpack(&t, &spec(), home.path())
                .unwrap_err()
                .contains("連結")
        );
    }
}
