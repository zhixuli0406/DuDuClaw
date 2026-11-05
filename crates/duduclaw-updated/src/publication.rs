//! Publication policy shared by offline bootstrap staging and normal updates.
//! Staging verified bytes does not activate services or establish readiness.
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use sha2::{Digest, Sha256};
use crate::{Error, Result};

pub fn artifact_name(target: &InstallTarget) -> Result<String> {
    Ok(match target {
        InstallTarget::Gateway => "duduclaw".into(),
        InstallTarget::Spawnd => "duduclaw-spawnd".into(),
        InstallTarget::Trampoline => "duduclaw-run".into(),
        InstallTarget::Egress => "duduclaw-egress".into(),
        InstallTarget::EgressForwarder => "duduclaw-egress-forwarder".into(),
        InstallTarget::McpBridge => "duduclaw-mcp-bridge".into(),
        InstallTarget::ParseWorker => "duduclaw-parse-worker".into(),
        InstallTarget::Importer => "duduclaw-import".into(),
        InstallTarget::Migration => "duduclaw-migrate".into(),
        InstallTarget::UpdateSlot => "duduclaw-updated".into(),
        InstallTarget::RuntimeEntry { registry_id } => {
            if registry_id.is_empty() || registry_id.len() > 128 || !registry_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) { return Err(Error::Invalid); }
            format!("runtime-{registry_id}")
        },
    })
}

/// Validate a complete installer generation after authenticating its manifest.
/// Runtime registrations are a separate signed operation, never guessed here.
pub fn validate_bootstrap_manifest(manifest: &ReleaseManifest, arch: Architecture) -> Result<()> {
    manifest.validate()?;
    if manifest.product != Product::Duduclaw || manifest.arch != arch { return Err(Error::Incompatible); }
    let p = &manifest.protocols;
    if [p.gateway, p.spawnd, p.trampoline, p.egress, p.updated, p.mcp_bridge, p.parse_worker, p.importer]
        .iter().any(|value| *value != 1) { return Err(Error::Incompatible); }
    let required = [InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline,
        InstallTarget::Egress, InstallTarget::EgressForwarder, InstallTarget::McpBridge,
        InstallTarget::ParseWorker, InstallTarget::Importer, InstallTarget::Migration, InstallTarget::UpdateSlot];
    if manifest.artifacts.len() != required.len() { return Err(Error::Incompatible); }
    for target in required {
        if !manifest.artifacts.iter().any(|artifact| artifact.install_target == target
            && artifact.mode == 0o755 && artifact.size_bytes > 0 && artifact.size_bytes < 200 * 1024 * 1024) {
            return Err(Error::Incompatible);
        }
    }
    Ok(())
}

/// The pin argument comes only from the independently authenticated verifier's
/// compiled constant. The public CLI never accepts a pin or trust file path.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn authenticate(root_bytes: &[u8], root_signatures: &[String], pin: Sha256Digest,
    manifest_bytes: &[u8], signatures: &[String], arch: Architecture, now: u64) -> Result<ReleaseManifest> {
    let root = crate::trust::verify_root(root_bytes, root_signatures, pin, now)?;
    if signatures.len() != 1 { return Err(Error::Signature); }
    let manifest = crate::trust::verify_release(&root, manifest_bytes, &signatures[0], now)?;
    validate_bootstrap_manifest(&manifest, arch)?;
    Ok(manifest)
}

pub fn manifest_digest(bytes: &[u8]) -> Sha256Digest { Sha256Digest(Sha256::digest(bytes).into()) }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{metadata::canonical_bytes, sshsig::test_support::sign, trust_store::tests::{root, root_sign}};

    pub(crate) fn manifest() -> ReleaseManifest {
        let targets = [InstallTarget::Gateway, InstallTarget::Spawnd, InstallTarget::Trampoline,
            InstallTarget::Egress, InstallTarget::EgressForwarder, InstallTarget::McpBridge,
            InstallTarget::ParseWorker, InstallTarget::Importer, InstallTarget::Migration, InstallTarget::UpdateSlot];
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw,
            version: ReleaseVersion { major: 1, minor: 80, patch: 0 },
            minimum_version: ReleaseVersion { major: 1, minor: 79, patch: 3 }, serial: 1,
            channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
            created_at_unix: 100, expires_at_unix: 500,
            artifacts: targets.into_iter().map(|target| ReleaseArtifact {
                id: artifact_name(&target).unwrap(), sha256: manifest_digest(b"fixture"), size_bytes: 7,
                mode: 0o755, install_target: target }).collect(),
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1,
                updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] }
    }
    #[test]
    fn bootstrap_complete_generation_exact_modes_sizes_and_architecture() {
        let good = manifest();
        validate_bootstrap_manifest(&good, Architecture::X86_64).unwrap();
        assert!(validate_bootstrap_manifest(&good, Architecture::Aarch64).is_err());
        for index in 0..good.artifacts.len() {
            let mut bad = good.clone(); bad.artifacts.remove(index);
            assert!(validate_bootstrap_manifest(&bad, Architecture::X86_64).is_err());
            for mode in [0o644, 0o700, 0o775, 0o4755] {
                let mut bad = good.clone(); bad.artifacts[index].mode = mode;
                assert!(validate_bootstrap_manifest(&bad, Architecture::X86_64).is_err());
            }
            let mut bad = good.clone(); bad.artifacts[index].size_bytes = 200 * 1024 * 1024;
            assert!(validate_bootstrap_manifest(&bad, Architecture::X86_64).is_err());
        }
        let mut bad = good; bad.protocols.importer = 2;
        assert!(validate_bootstrap_manifest(&bad, Architecture::X86_64).is_err());
    }
    #[test]
    fn bootstrap_requires_independent_root_pin_threshold_and_target_signature() {
        let (root, keys, target) = root(7);
        let root_bytes = canonical_bytes(&root).unwrap();
        let roots = root_sign(&root, &keys, &root_bytes, ROOT_SSHSIG_NAMESPACE);
        let bytes = canonical_bytes(&manifest()).unwrap();
        let sig = vec![sign(&target, &root.target_key, RELEASE_SSHSIG_NAMESPACE, &bytes, 5)];
        let pin = manifest_digest(&root_bytes);
        authenticate(&root_bytes, &roots, pin, &bytes, &sig, Architecture::X86_64, 100).unwrap();
        assert!(authenticate(&root_bytes, &roots, Sha256Digest([0;32]), &bytes, &sig, Architecture::X86_64, 100).is_err());
        assert!(authenticate(&root_bytes, &roots[..1], pin, &bytes, &sig, Architecture::X86_64, 100).is_err());
        assert!(authenticate(&root_bytes, &roots, pin, &bytes, &[], Architecture::X86_64, 100).is_err());
        assert!(authenticate(&root_bytes, &roots, pin, &bytes, &[sig[0].clone(), sig[0].clone()], Architecture::X86_64, 100).is_err());
        assert!(authenticate(&root_bytes, &roots, pin, &bytes, &sig, Architecture::X86_64, 500).is_err());
        let forged = vec![sign(&keys[1], &root.root_keys[1], RELEASE_SSHSIG_NAMESPACE, &bytes, 5)];
        assert!(authenticate(&root_bytes, &roots, pin, &bytes, &forged, Architecture::X86_64, 100).is_err());
    }
    #[test]
    fn publication_names_match_shipped_executables() {
        assert_eq!(artifact_name(&InstallTarget::ParseWorker).unwrap(), "duduclaw-parse-worker");
        assert_eq!(artifact_name(&InstallTarget::Migration).unwrap(), "duduclaw-migrate");
        let names: std::collections::BTreeSet<_> = manifest().artifacts.into_iter()
            .map(|artifact| artifact_name(&artifact.install_target).unwrap()).collect();
        assert_eq!(names.len(), 10);
    }
}
