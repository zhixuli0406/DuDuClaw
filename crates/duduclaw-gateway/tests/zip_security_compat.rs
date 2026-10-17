//! Exercise the ZIP extraction API used by the Windows desktop updater.
//! Unix-specific tests additionally cover mode restoration and symlink targets.

use std::io::{Cursor, Write};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

#[test]
fn normal_update_archive_extracts_exact_payload() {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "update/setup.exe",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"MZ-fixture-only").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let destination = tempfile::tempdir().unwrap();
    ZipArchive::new(Cursor::new(bytes))
        .unwrap()
        .extract(destination.path())
        .unwrap();
    assert_eq!(
        std::fs::read(destination.path().join("update/setup.exe")).unwrap(),
        b"MZ-fixture-only"
    );
}

#[cfg(unix)]
#[test]
fn archive_permissions_cannot_restore_suid_sgid_or_sticky_bits() {
    use std::os::unix::fs::PermissionsExt;
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    // Inject raw attacker-controlled external attributes: unix_permissions()
    // deliberately masks these bits when creating a legitimate archive.
    writer
        .start_file(
            "payload",
            SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .external_attributes(0o107755 << 16),
        )
        .unwrap();
    writer.write_all(b"ordinary payload").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let destination = tempfile::tempdir().unwrap();
    ZipArchive::new(Cursor::new(bytes))
        .unwrap()
        .extract(destination.path())
        .unwrap();
    let mode = std::fs::metadata(destination.path().join("payload"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7000, 0);
    assert_eq!(mode & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn escaping_symlink_targets_are_refused_without_changing_outside_files() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    std::fs::write(&outside, b"keep outside content").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    for (index, target) in [
        "../outside".to_owned(),
        outside.to_str().unwrap().to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        let destination = root.path().join(format!("extract-{index}"));
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "link",
                target,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(ZipArchive::new(Cursor::new(bytes))
            .unwrap()
            .extract(&destination)
            .is_err());
        assert!(std::fs::symlink_metadata(destination.join("link")).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep outside content");
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn allowed_symlink_cannot_change_target_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let destination = tempfile::tempdir().unwrap();
    let target = destination.path().join("target");
    std::fs::write(&target, b"keep target content").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .add_symlink(
            "link",
            "target",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    ZipArchive::new(Cursor::new(bytes))
        .unwrap()
        .extract(destination.path())
        .unwrap();
    assert_eq!(
        std::fs::read_link(destination.path().join("link")).unwrap(),
        std::path::Path::new("target")
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"keep target content");
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
