use super::*;

fn manifest() -> ArtifactManifest {
    ArtifactManifest::from_checkpoint(BTreeMap::from([
        (PathBuf::from("docs"), "directory".into()),
        (PathBuf::from("docs/繁體.txt"), format!("{:x}", Sha256::digest(b"checkpoint"))),
    ])).unwrap()
}

#[test]
fn artifact_manifest_roundtrip_binds_exact_checkpoint_digest() {
    let manifest = manifest();
    // Golden checkpoint digest computed independently over UTF-8 filename byte
    // length/name/digest tuples; retains the legacy directory digest format.
    assert_eq!(manifest.digest(), "1d298ea4106b2173d7f4c0f53e054adc5317bae61388fbba7639a2952b3708b2");
    let encoded = manifest.encode().unwrap();
    let decoded = ArtifactManifest::decode(&encoded, &manifest.digest()).unwrap();
    assert_eq!(decoded.files().collect::<Vec<_>>(), manifest.files().collect::<Vec<_>>());
    assert!(ArtifactManifest::decode(&encoded, &"0".repeat(64)).is_err());
    let mut changed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    changed["root"] = serde_json::json!("/private");
    assert!(ArtifactManifest::decode(&changed.to_string(), &manifest.digest()).is_err());
}

#[test]
fn artifact_manifest_rejects_escape_ambiguous_ancestors_and_unbounded_data() {
    for path in ["/etc/passwd", "../secret", "docs/../secret", "docs//secret", "docs\\secret", "docs/\nsecret"] {
        assert!(ArtifactManifest::from_checkpoint(BTreeMap::from([
            (PathBuf::from(path), "a".repeat(64)),
        ])).is_err(), "{path}");
    }
    assert!(ArtifactManifest::from_checkpoint(BTreeMap::from([
        (PathBuf::from("docs/file"), "a".repeat(64)),
    ])).is_err());
    assert!(ArtifactManifest::from_checkpoint(BTreeMap::from([
        (PathBuf::from("docs"), "a".repeat(64)),
        (PathBuf::from("docs/file"), "b".repeat(64)),
    ])).is_err());
    let many = (0..=MAX_ENTRIES).map(|index| (PathBuf::from(format!("f{index}")), "a".repeat(64))).collect();
    assert!(ArtifactManifest::from_checkpoint(many).is_err());
    assert!(ArtifactManifest::decode(&" ".repeat(MAX_MANIFEST_BYTES + 1), &"a".repeat(64)).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn artifact_manifest_descriptor_delivery_rejects_symlinks_hardlinks_and_fifo() {
    use std::{io::Read, os::unix::fs::{PermissionsExt, symlink}};
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("discovery/artifacts/run/cell/ws");
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/result.txt"), "checkpoint").unwrap();
    let artifact = AuthorizedArtifact::open(home.path(), "run", "cell").unwrap();
    let mut text = String::new();
    artifact.open_file("docs/result.txt").unwrap().read_to_string(&mut text).unwrap();
    assert_eq!(text, "checkpoint");
    let secret = home.path().join("secret");
    std::fs::write(&secret, "MUST_NOT_BE_READ").unwrap();
    symlink(&secret, root.join("linked")).unwrap();
    assert!(artifact.open_file("linked").is_err());
    symlink(home.path(), root.join("linked-dir")).unwrap();
    assert!(artifact.open_file("linked-dir/secret").is_err());
    std::fs::hard_link(&secret, root.join("hardlinked")).unwrap();
    assert!(artifact.open_file("hardlinked").is_err());
    nix::unistd::mkfifo(&root.join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(artifact.open_file("fifo").is_err());
    assert!(artifact.open_file("../secret").is_err());
    std::fs::set_permissions(root.join("docs"), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(artifact.open_file("docs/result.txt").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn artifact_manifest_descriptor_root_rejects_replaced_ancestor() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("discovery/artifacts/run/cell")).unwrap();
    symlink(outside.path(), home.path().join("discovery/artifacts/run/cell/ws")).unwrap();
    assert!(AuthorizedArtifact::open(home.path(), "run", "cell").is_err());
    assert!(AuthorizedArtifact::open(home.path(), "../run", "cell").is_err());
}
