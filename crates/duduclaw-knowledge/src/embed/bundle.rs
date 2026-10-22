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

/// A regular file that is not a symlink (`symlink_metadata` does not follow).
fn plain_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file())
}

/// The bundle and signature for `spec` on this platform, if both are there
/// as plain files (a symlinked bundle or signature is not used).
pub fn find(spec: &ModelSpec, home: &Path) -> Option<(PathBuf, PathBuf)> {
    let tar = bundle_dir(home).join(bundle_name(spec, ort_manifest::current_target()));
    let mut sig = tar.clone().into_os_string();
    sig.push(".minisig");
    let sig = PathBuf::from(sig);
    (plain_file(&tar) && plain_file(&sig)).then_some((tar, sig))
}

/// Check `sig_text` over everything `data` yields, with `pubkey_b64`, and
/// that the trusted comment names `expected_name` as one of its words.
pub fn verify_signature_from(
    data: &mut impl Read,
    sig_text: &str,
    pubkey_b64: &str,
    expected_name: &str,
) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("發行公鑰無效：{e}"))?;
    let sig =
        minisign_verify::Signature::decode(sig_text).map_err(|_| "簽章檔格式不正確".to_string())?;
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
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = data
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

/// [`verify_signature_from`] over the file at `tar`.
pub fn verify_signature(
    tar: &Path,
    sig_path: &Path,
    pubkey_b64: &str,
    expected_name: &str,
) -> Result<(), String> {
    let sig_text = std::fs::read_to_string(sig_path).map_err(|e| format!("無法讀取簽章檔：{e}"))?;
    let mut f = std::fs::File::open(tar).map_err(|e| format!("無法讀取安裝包：{e}"))?;
    verify_signature_from(&mut f, &sig_text, pubkey_b64, expected_name)
}

/// Verify and unpack through ONE open handle: the signature is checked over
/// the bytes of that handle, which is rewound and unpacked, so the file
/// cannot be swapped between the check and the unpack.
pub fn verify_and_unpack(
    tar: &Path,
    sig_path: &Path,
    pubkey_b64: &str,
    expected_name: &str,
    spec: &ModelSpec,
    home: &Path,
) -> Result<usize, String> {
    use std::io::{Seek, SeekFrom};
    if !plain_file(tar) || !plain_file(sig_path) {
        return Err("離線安裝包或簽章檔不是一般檔案".to_string());
    }
    let sig_text = std::fs::read_to_string(sig_path).map_err(|e| format!("無法讀取簽章檔：{e}"))?;
    let mut f = std::fs::File::open(tar).map_err(|e| format!("無法讀取安裝包：{e}"))?;
    verify_signature_from(&mut f, &sig_text, pubkey_b64, expected_name)?;
    f.seek(SeekFrom::Start(0))
        .map_err(|e| format!("無法讀取安裝包：{e}"))?;
    unpack_from(f, spec, home)
}

/// Relative path inside the tar → (destination, pinned size), for
/// everything a bundle of `spec` may contain on this platform.
fn expected_entries(spec: &ModelSpec, home: &Path) -> HashMap<String, (PathBuf, u64)> {
    let mut map = HashMap::new();
    let dir = spec.install_dir(home);
    for f in &spec.files {
        map.insert(
            format!("models/embedding/{}/{}", spec.id, f.path),
            (ModelSpec::dest(&dir, f), f.size),
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
            (duduclaw_onnx::install::ort_archive_path(&root, a), a.size),
        );
    }
    map
}

/// Unpack the bundle at `tar` (see [`unpack_from`]).
pub fn unpack(tar: &Path, spec: &ModelSpec, home: &Path) -> Result<usize, String> {
    let f = std::fs::File::open(tar).map_err(|e| format!("無法讀取安裝包：{e}"))?;
    unpack_from(f, spec, home)
}

/// Write one entry to `<dest>.part` (at most `size` bytes), then rename. The
/// `.part` file is removed on any failure.
fn write_entry(entry: &mut impl Read, dest: &Path, size: u64, path: &str) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part = duduclaw_onnx::install::part_path(dest);
    let result = (|| -> Result<(), String> {
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry.take(size + 1), &mut out)
            .map_err(|e| format!("解開安裝包失敗：{e}"))?;
        if copied > size {
            return Err(format!("安裝包內的 {path} 比預期大，已拒絕"));
        }
        drop(out);
        std::fs::rename(&part, dest).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// Unpack a verified bundle: only the expected regular files, each capped at
/// its pinned size, written to a `.part` file and renamed into place.
pub fn unpack_from(reader: impl Read, spec: &ModelSpec, home: &Path) -> Result<usize, String> {
    let expected = expected_entries(spec, home);
    let manifest = format!("models/embedding/{}/bundle.json", spec.id);
    let mut archive = tar::Archive::new(reader);
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
        let Some((dest, size)) = expected.get(&path) else {
            return Err(format!("安裝包內有預期以外的檔案：{path}"));
        };
        write_entry(&mut entry, dest, *size, &path)?;
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
    fn an_entry_larger_than_its_pin_is_refused_and_leaves_nothing() {
        let home = tempfile::tempdir().unwrap();
        let s = spec(); // m.onnx pinned at 5 bytes
        let tar = home.path().join("big.tar");
        tar_with(
            &[("models/embedding/tiny/onnx/m.onnx", b"model-but-longer")],
            &tar,
        );
        assert!(
            unpack(&tar, &s, home.path())
                .unwrap_err()
                .contains("比預期大")
        );
        let dest = s.install_dir(home.path()).join("onnx/m.onnx");
        assert!(!dest.exists());
        assert!(
            !duduclaw_onnx::install::part_path(&dest).exists(),
            ".part removed"
        );
    }

    #[test]
    fn verify_and_unpack_uses_one_handle_and_refuses_symlinks() {
        let home = tempfile::tempdir().unwrap();
        let s = spec();
        let tar = home.path().join("b.tar");
        tar_with(&[("models/embedding/tiny/onnx/m.onnx", b"model")], &tar);
        let (pk, sig) = sign(&std::fs::read(&tar).unwrap(), "duduclaw-kb-model t b.tar");
        let sig_path = home.path().join("b.tar.minisig");
        std::fs::write(&sig_path, &sig).unwrap();
        assert_eq!(
            verify_and_unpack(&tar, &sig_path, &pk, "b.tar", &s, home.path()).unwrap(),
            1
        );
        #[cfg(unix)]
        {
            let link = home.path().join("link.tar");
            std::os::unix::fs::symlink(&tar, &link).unwrap();
            assert!(verify_and_unpack(&link, &sig_path, &pk, "b.tar", &s, home.path()).is_err());
            // `find` ignores a symlinked bundle.
            let name = bundle_name(&s, ort_manifest::current_target());
            std::fs::create_dir_all(bundle_dir(home.path())).unwrap();
            std::os::unix::fs::symlink(&tar, bundle_dir(home.path()).join(&name)).unwrap();
            std::fs::write(
                bundle_dir(home.path()).join(format!("{name}.minisig")),
                &sig,
            )
            .unwrap();
            assert!(find(&s, home.path()).is_none());
        }
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
