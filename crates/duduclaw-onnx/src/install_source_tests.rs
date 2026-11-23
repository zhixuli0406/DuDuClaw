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
    extract_source_member(&path, &src, &dest, 4).unwrap().persist(&dest).unwrap();
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
    assert!(extract_source_member(&path, &src, &dest, 4).is_err());
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
        assert!(extract_source_member(&path, &src, &dest, 4).is_err(), "{bad}");
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
        let invalid = OrtSource { member: bad, ..src };
        assert!(extract_source_member(&path, &invalid, &dest, 4).is_err());
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
            (|| {
                let mut part = new_temp(&dest)?;
                part.write_all(&data)?;
                Ok(part)
            })()
        };
        std::future::ready(result)
    }).await.unwrap();
    assert_eq!(calls, ["first", "second"]);
    assert_eq!(std::fs::read(lib_dir.join("lib.so")).unwrap(), b"good");
}

#[tokio::test]
async fn fallback_progress_uses_second_source_size_and_logs_success() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib_dir = manifest::ort_lib_dir(root);
    std::fs::create_dir_all(&lib_dir).unwrap();
    let first_bytes = zip_bytes(&[("lib.so", b"good")]);
    let second_bytes = zip_bytes(&[("lib.so", b"good"), ("padding", &[0u8; 96])]);
    let first = source(&first_bytes, "lib.so", "https://github.com/first?token=secret");
    let second = source(&second_bytes, "lib.so", "https://api.nuget.org/second?token=secret");
    let spec = archive(first, second);
    let pins = [(4, hex_lower(&Sha256::digest(b"good")))];
    let pins = [(pins[0].0, pins[0].1.as_str())];
    let cancel = Arc::new(AtomicBool::new(false));
    // Exercise the same source loop with the existing fake-fetcher seam.
    let mut reports = Vec::new();
    let reserved = max_source_size(&spec);
    install_from_sources(root, &spec, &pins, &cancel, |src, dest| {
        let data = if src.url == first.url { None } else { Some(second_bytes.clone()) };
        reports.push(0);
        let result = if let Some(data) = data {
            let mut part = new_temp(&dest).unwrap();
            part.write_all(&data).unwrap();
            reports.push(((data.len() as u128 * reserved as u128) / src.size as u128) as u64);
            Ok(part)
        } else {
            Err(OnnxError::msg("HTTP 404"))
        };
        std::future::ready(result)
    }).await.unwrap();

    assert_eq!(reserved, second.size);
    reports.push(reserved);
    assert!(reports.iter().all(|value| *value <= reserved));
    assert_eq!(reports.last(), Some(&reserved));
    assert_eq!(std::fs::read(lib_dir.join("lib.so")).unwrap(), b"good");
    // install_from_sources' success record is asserted through its pure
    // formatting helper so the test does not depend on global stderr capture.
    let record = super::success_record(spec.sources().nth(1).unwrap());
    assert_eq!(record, "ONNX Runtime 來源成功：kind=Zip host=api.nuget.org");
    assert!(!record.contains("secret"));
}

#[tokio::test]
async fn first_smaller_source_success_fills_reserved_progress_slot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let lib_dir = manifest::ort_lib_dir(root);
    std::fs::create_dir_all(&lib_dir).unwrap();
    let small_bytes = zip_bytes(&[("lib.so", b"good")]);
    let large_bytes = zip_bytes(&[("lib.so", b"good"), ("padding", &[0u8; 96])]);
    let first = source(&small_bytes, "lib.so", "small");
    let second = source(&large_bytes, "lib.so", "large");
    let spec = archive(first, second);
    let pins = [(4, hex_lower(&Sha256::digest(b"good")))];
    let pins = [(pins[0].0, pins[0].1.as_str())];
    let cancel = Arc::new(AtomicBool::new(false));
    let reserved = max_source_size(&spec);
    let mut reports = vec![0, ((first.size as u128 * reserved as u128) / first.size as u128) as u64];
    install_from_sources(root, &spec, &pins, &cancel, |src, dest| {
        let mut part = new_temp(&dest).unwrap();
        part.write_all(&small_bytes).unwrap();
        assert_eq!(src.url, "small");
        std::future::ready(Ok(part))
    }).await.unwrap();
    reports.push(reserved);
    assert!(reports.iter().all(|value| *value <= reserved));
    assert_eq!(reports.last(), Some(&reserved));
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

fn malicious_archive(kind: ArchiveKind, case: &str) -> Vec<u8> {
    match kind {
        ArchiveKind::TarGz => {
            let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut tar = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o600);
            if case == "symlink" || case == "hardlink" || case == "fifo" {
                header.set_size(0);
                header.set_entry_type(match case {
                    "symlink" => tar::EntryType::Symlink,
                    "hardlink" => tar::EntryType::Link,
                    _ => tar::EntryType::Fifo,
                });
                if case != "fifo" { header.set_link_name("elsewhere").unwrap(); }
                header.set_cksum();
                tar.append_data(&mut header, "lib.so", &[][..]).unwrap();
            } else {
                let bytes = if case == "oversize" { b"good-extra".as_slice() } else { b"good".as_slice() };
                header.set_size(bytes.len() as u64);
                header.set_cksum();
                tar.append_data(&mut header, "lib.so", bytes).unwrap();
                if case == "duplicate" {
                    tar.append_data(&mut header, "lib.so", bytes).unwrap();
                }
            }
            tar.into_inner().unwrap().finish().unwrap()
        }
        ArchiveKind::Zip => {
            if case == "symlink" {
                let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
                zip.add_symlink("lib.so", "elsewhere", zip::write::SimpleFileOptions::default()).unwrap();
                return zip.finish().unwrap().into_inner();
            }
            if case == "duplicate" {
                // ZipWriter refuses duplicates itself; rename an equal-length
                // fixture name in both the local and central headers instead.
                let mut bytes = zip_bytes(&[("lib.so", b"good"), ("alt.so", b"evil")]);
                for i in 0..bytes.len().saturating_sub(5) {
                    if bytes.get(i..i + 6) == Some(b"alt.so") {
                        bytes[i..i + 6].copy_from_slice(b"lib.so");
                    }
                }
                return bytes;
            }
            zip_bytes(&[("lib.so", if case == "oversize" { b"good-extra" } else { b"good" })])
        }
    }
}

fn refused_member_case(case: &str) {
    for kind in [ArchiveKind::TarGz, ArchiveKind::Zip] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive");
        let dest = dir.path().join("lib.so");
        let bytes = malicious_archive(kind, case);
        let src = OrtSource { kind, ..source(&bytes, "lib.so", "fixture") };
        std::fs::write(&path, bytes).unwrap();
        // A rejected replacement preserves the previous install and leaves no partial.
        std::fs::write(&dest, b"old").unwrap();
        assert!(extract_source_member(&path, &src, &dest, 4).is_err(), "{kind:?}: {case}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

#[test]
fn tar_and_zip_refuse_oversized_member_and_remove_partial() { refused_member_case("oversize"); }

#[test]
fn tar_and_zip_refuse_duplicate_member_names() { refused_member_case("duplicate"); }

#[test]
fn tar_and_zip_refuse_symlink_member() { refused_member_case("symlink"); }

#[test]
fn tar_refuses_hardlink_and_special_member() {
    let dir = tempfile::tempdir().unwrap();
    for case in ["hardlink", "fifo"] {
        let bytes = malicious_archive(ArchiveKind::TarGz, case);
        let src = OrtSource { kind: ArchiveKind::TarGz, ..source(&bytes, "lib.so", "fixture") };
        let path = dir.path().join("archive");
        std::fs::write(&path, bytes).unwrap();
        assert!(extract_source_member(&path, &src, &dir.path().join("lib.so"), 4).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn bounded_extraction_accepts_exact_limit_for_tar_and_zip() {
    for kind in [ArchiveKind::TarGz, ArchiveKind::Zip] {
        let dir = tempfile::tempdir().unwrap();
        let bytes = malicious_archive(kind, "valid");
        let src = OrtSource { kind, ..source(&bytes, "lib.so", "fixture") };
        let path = dir.path().join("archive");
        let dest = dir.path().join("lib.so");
        std::fs::write(&path, bytes).unwrap();
        let hash = hex_lower(&Sha256::digest(b"good"));
        extract_verified(&path, &src, &dest, &[(4, &hash)]).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"good");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

#[cfg(unix)]
#[test]
fn precreated_part_symlinks_are_never_followed_or_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("lib.so");
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"untouched").unwrap();
    let old = part_path(&dest);
    let random = dir.path().join(".lib.so.dudu-abcdefghijklmnop.part");
    for link in [&old, &random] {
        std::os::unix::fs::symlink(&victim, link).unwrap();
        assert!(exclusive_temp(link).is_err());
    }
    cleanup_stale_temps(&dest);
    let mut part = new_temp(&dest).unwrap();
    assert_ne!(part.path(), old);
    part.write_all(b"good").unwrap();
    finish_download(part, &dest, 4, &hex_lower(&Sha256::digest(b"good"))).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
    assert!(std::fs::symlink_metadata(old).unwrap().file_type().is_symlink());
    assert!(std::fs::symlink_metadata(random).unwrap().file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn stale_cleanup_only_removes_owned_regular_matching_files() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("lib.so");
    let stale = dir.path().join(".lib.so.dudu-abcdefghijklmnop.part");
    let unrelated = dir.path().join("other.part");
    let fresh = dir.path().join(".lib.so.dudu-qrstuvwxyzABCDEF.part");
    let directory = dir.path().join(".lib.so.dudu-0123456789abcdef.part");
    for path in [&stale, &unrelated, &fresh] { std::fs::write(path, b"old").unwrap(); }
    for path in [&stale, &unrelated] {
        std::fs::File::options().write(true).open(path).unwrap()
            .set_modified(std::time::UNIX_EPOCH).unwrap();
    }
    std::fs::create_dir(&directory).unwrap();
    cleanup_stale_temps(&dest);
    assert!(!stale.exists());
    assert!(unrelated.exists() && fresh.exists() && directory.is_dir());
}

#[test]
fn redirect_policy_allows_only_source_hosts_https_and_five_hops() {
    let github = reqwest::Url::parse(manifest::ORT_ARCHIVES[0].url).unwrap();
    let nuget = reqwest::Url::parse(manifest::ORT_ARCHIVES[0].fallbacks[0].url).unwrap();
    for host in manifest::GITHUB_REDIRECT_HOSTS {
        let url = reqwest::Url::parse(&format!("https://{host}/asset")).unwrap();
        assert!(redirect_allowed(&github, &url, 5));
        assert!(!redirect_allowed(&github, &url, 6));
        assert!(!redirect_allowed(&nuget, &url, 1));
    }
    assert!(redirect_allowed(&nuget, &nuget, 1));
    for url in ["http://github.com/asset", "https://github.com.evil.test/asset",
        "https://evil.test/asset", "https://github.com:444/asset", "https://user@github.com/asset",
        "file:///tmp/asset", "https://api.nuget.org/asset"] {
        assert!(!redirect_allowed(&github, &reqwest::Url::parse(url).unwrap(), 1), "{url}");
    }
    let models = reqwest::Url::parse("https://huggingface.co/model").unwrap();
    let cdn = reqwest::Url::parse("https://cas-bridge.xethub.hf.co/asset").unwrap();
    assert!(redirect_allowed(&models, &cdn, 1));
    assert!(!redirect_allowed(&nuget, &cdn, 1));
}

#[test]
fn redirect_policy_allows_hf_subdomains_only_for_huggingface() {
    let models = reqwest::Url::parse("https://huggingface.co/model").unwrap();
    let github = reqwest::Url::parse(manifest::ORT_ARCHIVES[0].url).unwrap();
    let nuget = reqwest::Url::parse(manifest::ORT_ARCHIVES[0].fallbacks[0].url).unwrap();
    let unknown = reqwest::Url::parse("https://unknown.example/model").unwrap();
    for host in ["us.aws.cdn.hf.co", "eu.aws.cdn.hf.co", "cas-bridge.xethub.hf.co"] {
        let url = reqwest::Url::parse(&format!("https://{host}/asset")).unwrap();
        assert!(redirect_allowed(&models, &url, 5), "{host}");
        assert!(!redirect_allowed(&models, &url, 6), "{host}");
        assert!(!redirect_allowed(&github, &url, 1), "{host}");
        assert!(!redirect_allowed(&nuget, &url, 1), "{host}");
        assert!(!redirect_allowed(&unknown, &url, 1), "{host}");
    }
}

#[test]
fn redirect_policy_refuses_hf_lookalikes_and_unsafe_urls() {
    let models = reqwest::Url::parse("https://huggingface.co/model").unwrap();
    for url in ["https://hf.co.evil.com/asset", "https://evilhf.co/asset",
        "https://xhf.co/asset", "https://hf.co/asset", "http://us.aws.cdn.hf.co/asset",
        "https://us.aws.cdn.hf.co:8443/asset", "https://user@us.aws.cdn.hf.co/asset",
        "https://user:password@us.aws.cdn.hf.co/asset", "https://:password@us.aws.cdn.hf.co/asset"] {
        assert!(!redirect_allowed(&models, &reqwest::Url::parse(url).unwrap(), 1), "{url}");
    }
}
