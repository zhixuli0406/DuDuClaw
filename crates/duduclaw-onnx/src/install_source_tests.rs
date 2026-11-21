use super::*;
use std::io::Cursor;

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for &(name, bytes) in entries {
        zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn source(bytes: &[u8], member: &'static str, url: &'static str) -> OrtSource {
    OrtSource {
        url, size: bytes.len() as u64,
        sha256: Box::leak(hex_lower(&Sha256::digest(bytes)).into_boxed_str()),
        kind: ArchiveKind::Zip, member,
    }
}

fn archive(first: OrtSource, second: OrtSource) -> OrtArchive {
    OrtArchive {
        target: "test", lib_file: "lib.so",
        url: first.url, size: first.size, sha256: first.sha256,
        kind: first.kind, member: first.member,
        fallbacks: Box::leak(vec![second].into_boxed_slice()),
    }
}

#[test]
fn zip_extracts_only_the_exact_pinned_member() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = zip_bytes(&[("runtimes/linux-x64/native/lib.so", b"good"), ("other", b"ignored")]);
    let src = source(&bytes, "runtimes/linux-x64/native/lib.so", "first");
    let path = dir.path().join("package.zip");
    let dest = dir.path().join("lib.so");
    std::fs::write(&path, bytes).unwrap();
    extract_source_member(&path, &src, &dest).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"good");
    assert!(!dir.path().join("other").exists());
    assert!(!dir.path().join("runtimes").exists());
}

#[test]
fn zip_refuses_a_different_member_path() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = zip_bytes(&[("runtimes/linux-arm64/native/lib.so", b"good")]);
    let src = source(&bytes, "runtimes/linux-x64/native/lib.so", "first");
    let path = dir.path().join("package.zip");
    let dest = dir.path().join("lib.so");
    std::fs::write(&path, bytes).unwrap();
    assert!(extract_source_member(&path, &src, &dest).is_err());
    assert!(!dest.exists());
    assert!(!dest.with_extension("part").exists());
}

#[test]
fn zip_refuses_parent_components_even_in_unselected_entries() {
    for bad in ["../outside", "runtimes/../lib.so", "runtimes\\..\\lib.so"] {
        let dir = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[("lib.so", b"good"), (bad, b"evil")]);
        let src = source(&bytes, "lib.so", "first");
        let path = dir.path().join("package.zip");
        let dest = dir.path().join("lib.so");
        std::fs::write(&path, bytes).unwrap();
        assert!(extract_source_member(&path, &src, &dest).is_err(), "{bad}");
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
        let invalid = OrtSource { member: bad, ..src };
        assert!(extract_source_member(&path, &invalid, &dest).is_err());
    }
}

async fn fallback_case(first_error: Option<&str>, corrupt_first: bool) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib_dir = manifest::ort_lib_dir(root);
    std::fs::create_dir_all(&lib_dir).unwrap();
    let bytes = zip_bytes(&[("lib.so", b"good")]);
    let spec = archive(source(&bytes, "lib.so", "first"), source(&bytes, "lib.so", "second"));
    let pins = [(4, hex_lower(&Sha256::digest(b"good")))];
    let pins = [(pins[0].0, pins[0].1.as_str())];
    let cancel = Arc::new(AtomicBool::new(false));
    let mut calls = Vec::new();
    install_from_sources(root, &spec, &pins, &cancel, |src, dest| {
        calls.push(src.url);
        // The first source must not publish or leave any extracted bytes.
        assert!(!lib_dir.join("lib.so").exists());
        assert!(!lib_dir.join("lib.verified").exists());
        assert!(!lib_dir.join("lib.part").exists());
        let result = if src.url == "first" && first_error.is_some() {
            Err(OnnxError::msg(first_error.unwrap()))
        } else {
            let mut data = bytes.clone();
            if src.url == "first" && corrupt_first {
                // Same size, different digest. It must never reach the zip reader.
                data[0] ^= 1;
            }
            std::fs::write(part_path(&dest), data).map_err(OnnxError::from)
        };
        std::future::ready(result)
    }).await.unwrap();
    assert_eq!(calls, ["first", "second"]);
    assert_eq!(std::fs::read(lib_dir.join("lib.so")).unwrap(), b"good");
}

#[tokio::test]
async fn source_404_falls_back_in_order() {
    fallback_case(Some("HTTP 404"), false).await;
}

#[tokio::test]
async fn source_wrong_hash_falls_back_without_extracting() {
    fallback_case(None, true).await;
}

#[tokio::test]
async fn all_source_failures_include_each_source_and_reason() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = zip_bytes(&[("lib.so", b"good")]);
    let spec = archive(source(&bytes, "lib.so", "first"), source(&bytes, "lib.so", "second"));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut calls = Vec::new();
    let error = install_from_sources(dir.path(), &spec, &[], &cancel, |src, _| {
        calls.push(src.url);
        std::future::ready(Err(OnnxError::msg(if src.url == "first" { "HTTP 404" } else { "timeout" })))
    }).await.unwrap_err().to_string();
    assert_eq!(calls, ["first", "second"]);
    assert!(error.contains("first: HTTP 404"), "{error}");
    assert!(error.contains("second: timeout"), "{error}");
}
