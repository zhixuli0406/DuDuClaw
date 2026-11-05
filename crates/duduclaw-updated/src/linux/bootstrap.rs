//! Root-only, fixed-inbox publication of a verified but INACTIVE generation.
//! This deliberately cannot bootstrap trust/state/slots, select current, execute
//! an artifact or certify migration/readiness. Legacy transition dual-signature
//! verification and live activation must finish before those operations exist.
use std::{fs, io::{Seek, SeekFrom, Write}, os::unix::fs::{MetadataExt, PermissionsExt}, path::Path};
use duduclaw_isolation::{ids::Sha256Digest, update::{Architecture, ReleaseManifest}};
use crate::{Error, Result, publication::{artifact_name, manifest_digest}};
use super::{files::{Directory, verify_fd, copy_artifact}, CONFIG_ROOT, INSTALL_ROOT, STATE_ROOT};

pub const BOOTSTRAP_INBOX: &str = "/etc/duduclaw-updated/bootstrap";
const STAGE: &str = "bootstrap-generation.next";

fn missing(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}
fn host_architecture() -> Result<Architecture> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(Architecture::X86_64),
        "aarch64" => Ok(Architecture::Aarch64),
        _ => Err(Error::Incompatible),
    }
}
fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(Error::Maintenance),
    }
}

pub fn stage() -> Result<Sha256Digest> {
    if !nix::unistd::getuid().is_root() { return Err(Error::Maintenance); }
    super::harden()?;
    // A bootstrap publisher must never repair/overwrite an already managed
    // installation. It cannot know whether that state is currently recoverable.
    let state = Directory::open(Path::new(STATE_ROOT))?;
    let _lock = state.lock()?;
    absent(&Path::new(STATE_ROOT).join("state.json"))?;
    absent(&Path::new(CONFIG_ROOT).join("trust.json"))?;
    absent(&Path::new(INSTALL_ROOT).join("current"))?;
    let inbox = Directory::open(Path::new(BOOTSTRAP_INBOX))?;
    let root_bytes = inbox.bytes("root.json", crate::metadata::MAX_METADATA_BYTES)?;
    let root_signatures: Vec<String> = serde_json::from_slice(&inbox.bytes("root.json.signatures.json", 64 * 1024)?)?;
    let now = super::now()?;
    // No caller/environment pin; the verifier binary carries the independent pin.
    let trust = super::launcher::verify_bootstrap_root(&root_bytes, &root_signatures, now)?;
    let bytes = inbox.bytes("manifest.json", crate::metadata::MAX_METADATA_BYTES)?;
    let signatures: Vec<String> = serde_json::from_slice(&inbox.bytes("manifest.json.signatures.json", 16 * 1024)?)?;
    let manifest = crate::publication::authenticate(&root_bytes, &root_signatures, trust.pin,
        &bytes, &signatures, host_architecture()?, now)?;
    publish_generation(&inbox.child("artifacts")?, &Directory::open(Path::new(INSTALL_ROOT))?,
        &manifest, &bytes, &signatures[0])
}

fn verify_generation(generation: &Directory, manifest: &ReleaseManifest, bytes: &[u8], signature: &str) -> Result<()> {
    let mut expected = vec!["manifest.json".to_string(), "manifest.json.sig".to_string()];
    for artifact in &manifest.artifacts {
        let name = artifact_name(&artifact.install_target)?;
        let file = generation.read(&name)?;
        if file.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(Error::Signature); }
        verify_fd(file, artifact.size_bytes, artifact.sha256)?;
        expected.push(name);
    }
    let mut actual = generation.entries(duduclaw_isolation::update::MAX_ARTIFACTS + 2)?;
    expected.sort(); actual.sort();
    if actual != expected || generation.bytes("manifest.json", crate::metadata::MAX_METADATA_BYTES)? != bytes
        || generation.bytes("manifest.json.sig", 16 * 1024)? != signature.as_bytes() { return Err(Error::Signature); }
    Ok(())
}

fn publish_generation(artifacts: &Directory, install: &Directory, manifest: &ReleaseManifest,
    bytes: &[u8], signature: &str) -> Result<Sha256Digest> {
    // Open and verify the whole set before changing any destination. Retain those
    // fds through the copy; never reopen an input by name after hashing it.
    let mut sources = Vec::new();
    for artifact in &manifest.artifacts {
        let mut source = verify_fd(artifacts.read(&artifact.id)?, artifact.size_bytes, artifact.sha256)?;
        source.seek(SeekFrom::Start(0))?;
        sources.push(source);
    }
    let digest = manifest_digest(bytes);
    let name = hex::encode(digest.0);
    let generations = install.child("generations")?;
    match generations.child(&name) {
        Ok(generation) => { verify_generation(&generation, manifest, bytes, signature)?; return Ok(digest); }
        Err(error) if missing(&error) => (),
        Err(error) => return Err(error),
    }
    let total = manifest.artifacts.iter().try_fold(0u64, |n, artifact| n.checked_add(artifact.size_bytes)).ok_or(Error::Invalid)?;
    install.reserve_space(total)?;
    // A prior failed stage has no selector or authority. Cleanup is bounded and
    // rejects links, mounts and unexpected directories instead of recursive rm.
    match install.remove_artifact_directory(STAGE) {
        Ok(()) => (), Err(error) if missing(&error) => (), Err(error) => return Err(error),
    }
    let stage = install.create_child(STAGE)?;
    for (artifact, mut source) in manifest.artifacts.iter().zip(sources) {
        let mut output = stage.create(&artifact_name(&artifact.install_target)?)?;
        copy_artifact(&mut source, &mut output, artifact)?;
        output.set_permissions(fs::Permissions::from_mode(u32::from(artifact.mode)))?;
        output.sync_all()?;
    }
    for (name, content) in [("manifest.json", bytes), ("manifest.json.sig", signature.as_bytes())] {
        let mut file = stage.create(name)?; file.write_all(content)?; file.sync_all()?;
    }
    verify_generation(&stage, manifest, bytes, signature)?;
    // Only a complete, hash-verified generation becomes traversable by services.
    // This does not make it active: no fixed executable or selector is changed.
    // SAFETY: the directory descriptor is live and root-owned; mode is fixed.
    if unsafe { libc::fchmod(stage.raw_fd(), 0o755) } != 0 { return Err(std::io::Error::last_os_error().into()); }
    stage.sync()?;
    install.rename(STAGE, &generations, &name, false)?;
    verify_generation(&generations.child(&name)?, manifest, bytes, signature)?;
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires root native Linux openat2; uses only its own /run fixture"]
    fn iso_root_bootstrap_stage_is_idempotent_and_never_selects_or_commits() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("bootstrap-stage-").tempdir_in("/run").unwrap();
        let root = Directory::open(temp.path()).unwrap();
        let artifacts = root.create_child("artifacts").unwrap();
        root.create_child("generations").unwrap();
        let manifest = crate::publication::tests::manifest();
        for artifact in &manifest.artifacts {
            artifacts.create(&artifact.id).unwrap().write_all(b"fixture").unwrap();
        }
        let bytes = crate::metadata::canonical_bytes(&manifest).unwrap();
        let first = &manifest.artifacts[0];
        artifacts.remove(&first.id).unwrap();
        artifacts.create(&first.id).unwrap().write_all(b"corrupt").unwrap();
        assert!(publish_generation(&artifacts, &root, &manifest, &bytes, "fixture-signature").is_err());
        assert!(root.child("generations").unwrap().entries(1).unwrap().is_empty());
        assert!(!temp.path().join(STAGE).exists());
        artifacts.remove(&first.id).unwrap();
        artifacts.create(&first.id).unwrap().write_all(b"fixture").unwrap();
        // Recover an interrupted, unpublished copy without touching a selector.
        root.create_child(STAGE).unwrap().create("partial").unwrap().write_all(b"partial").unwrap();
        let digest = publish_generation(&artifacts, &root, &manifest, &bytes, "fixture-signature").unwrap();
        assert_eq!(publish_generation(&artifacts, &root, &manifest, &bytes, "fixture-signature").unwrap(), digest);
        for absent in ["current", "state.json", "trust.json", "slots", "slot-a.json"] {
            assert!(!temp.path().join(absent).exists());
        }
        let name = hex::encode(digest.0);
        let generation = root.child("generations").unwrap().child(&name).unwrap();
        let binary = artifact_name(&manifest.artifacts[0].install_target).unwrap();
        generation.remove(&binary).unwrap();
        assert!(publish_generation(&artifacts, &root, &manifest, &bytes, "fixture-signature").is_err());
    }

    #[test]
    #[ignore = "requires root native Linux openat2; uses only its own /run fixture"]
    fn iso_root_bootstrap_missing_or_linked_input_never_publishes() {
        use std::os::unix::fs::symlink;
        assert_eq!(nix::unistd::geteuid().as_raw(), 0);
        let temp = tempfile::Builder::new().prefix("bootstrap-reject-").tempdir_in("/run").unwrap();
        let root = Directory::open(temp.path()).unwrap();
        let artifacts = root.create_child("artifacts").unwrap();
        root.create_child("generations").unwrap();
        let manifest = crate::publication::tests::manifest();
        let bytes = crate::metadata::canonical_bytes(&manifest).unwrap();
        for artifact in &manifest.artifacts {
            artifacts.create(&artifact.id).unwrap().write_all(b"fixture").unwrap();
        }
        for artifact in &manifest.artifacts {
            artifacts.remove(&artifact.id).unwrap();
            assert!(publish_generation(&artifacts, &root, &manifest, &bytes, "fixture-signature").is_err());
            assert!(!temp.path().join(STAGE).exists());
            let foreign = temp.path().join("foreign");
            fs::write(&foreign, b"fixture").unwrap();
            symlink(&foreign, temp.path().join("artifacts").join(&artifact.id)).unwrap();
            assert!(publish_generation(&artifacts, &root, &manifest, &bytes, "fixture-signature").is_err());
            artifacts.remove(&artifact.id).unwrap();
            artifacts.create(&artifact.id).unwrap().write_all(b"fixture").unwrap();
        }
        assert!(root.child("generations").unwrap().entries(1).unwrap().is_empty());
    }
}
