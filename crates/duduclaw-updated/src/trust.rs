//! Root pinning, threshold rotation and role-separated release authentication.
use std::collections::BTreeSet;
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use sha2::{Digest, Sha256};

use crate::{metadata::decode_canonical, sshsig::{validate_key, SshSignature}, Error, Result};

pub fn validate_root(root: &RootMetadata, now: u64) -> Result<()> {
    root.validate()?;
    if now >= root.expires_at_unix { return Err(Error::Expired); }
    // At least one hardware root permits future rotations to more hardware keys.
    if !root.root_keys.iter().any(|key| key.algorithm == SigningAlgorithm::SkEd25519) {
        return Err(Error::Signature);
    }
    for key in root.root_keys.iter().chain(std::iter::once(&root.target_key)) { validate_key(key)?; }
    Ok(())
}

fn signatures(armors: &[String]) -> Result<Vec<SshSignature>> {
    if armors.is_empty() || armors.len() > 6 { return Err(Error::Signature); }
    let mut seen = BTreeSet::new();
    let mut signatures = Vec::new();
    for armor in armors {
        let signature = SshSignature::parse(armor)?;
        if !seen.insert(signature.key_id()) { return Err(Error::Signature); }
        signatures.push(signature);
    }
    Ok(signatures)
}

fn threshold(root: &RootMetadata, signatures: &[SshSignature], namespace: &str, bytes: &[u8]) -> Result<()> {
    let mut count = 0;
    for signature in signatures {
        if let Some(key) = root.root_keys.iter().find(|key| key.key_id == signature.key_id()) {
            signature.verify(key, namespace, bytes)?;
            count += 1;
        }
    }
    if count < usize::from(root.root_threshold) { return Err(Error::Signature); }
    Ok(())
}

fn reject_unknown(signatures: &[SshSignature], roots: &[&RootMetadata]) -> Result<()> {
    if signatures.iter().any(|signature| !roots.iter().any(|root|
        root.root_keys.iter().any(|key| key.key_id == signature.key_id()))) {
        return Err(Error::Signature);
    }
    Ok(())
}

/// Initial root must match a fingerprint independently pinned by the bootstrap verifier.
/// Passing a digest extracted from the same download would not establish trust.
pub fn verify_root(bytes: &[u8], armors: &[String], pinned_digest: Sha256Digest, now: u64) -> Result<RootMetadata> {
    if Sha256Digest(Sha256::digest(bytes).into()) != pinned_digest { return Err(Error::Signature); }
    let root: RootMetadata = decode_canonical(bytes)?;
    validate_root(&root, now)?;
    let signatures = signatures(armors)?;
    reject_unknown(&signatures, &[&root])?;
    threshold(&root, &signatures, ROOT_SSHSIG_NAMESPACE, bytes)?;
    Ok(root)
}

/// The caller durably stores the accepted root before trusting its target key.
/// Frozen roots can rotate to unfreeze; they cannot authorize target updates.
pub fn rotate_root(old: &RootMetadata, bytes: &[u8], armors: &[String], now: u64) -> Result<RootMetadata> {
    validate_root(old, now)?;
    let new: RootMetadata = decode_canonical(bytes)?;
    validate_root(&new, now)?;
    if old.version.checked_add(1) != Some(new.version) { return Err(Error::Rollback); }
    // Revocations are permanent: a later rotation cannot resurrect a key.
    if old.revoked_key_ids.iter().any(|id| !new.revoked_key_ids.iter().any(|new_id| id == new_id)) {
        return Err(Error::Signature);
    }
    let signatures = signatures(armors)?;
    reject_unknown(&signatures, &[old, &new])?;
    threshold(old, &signatures, ROOT_SSHSIG_NAMESPACE, bytes)?;
    threshold(&new, &signatures, ROOT_SSHSIG_NAMESPACE, bytes)?;
    Ok(new)
}

pub fn verify_release(root: &RootMetadata, bytes: &[u8], armor: &str, now: u64) -> Result<ReleaseManifest> {
    validate_root(root, now)?;
    if root.frozen { return Err(Error::Maintenance); }
    let manifest: ReleaseManifest = decode_canonical(bytes)?;
    manifest.validate()?;
    if now >= manifest.expires_at_unix || now < manifest.created_at_unix { return Err(Error::Expired); }
    SshSignature::parse(armor)?.verify(&root.target_key, RELEASE_SSHSIG_NAMESPACE, bytes)?;
    Ok(manifest)
}

/// Revalidate a previously accepted slot without treating an update freeze as
/// revocation of installed code. Its receipt is root-owned and digest-bound to
/// the committed/pending state. Current root validity and target key still apply.
pub fn verify_installed_release(root: &RootMetadata, bytes: &[u8], armor: &str,
    accepted_at: u64, now: u64) -> Result<ReleaseManifest> {
    validate_root(root, now)?;
    if accepted_at > now { return Err(Error::Signature); }
    let manifest: ReleaseManifest = decode_canonical(bytes)?;
    manifest.validate()?;
    if accepted_at < manifest.created_at_unix || accepted_at >= manifest.expires_at_unix { return Err(Error::Expired); }
    SshSignature::parse(armor)?.verify(&root.target_key, RELEASE_SSHSIG_NAMESPACE, bytes)?;
    Ok(manifest)
}

/// Offline/root-only emergency verification. `previously_verified` must come from
/// the root-owned history of target-verified manifests, never an RPC argument.
/// This does not install anything or relax the committed irreversible-migration gate.
pub fn verify_emergency(root: &RootMetadata, bytes: &[u8], armors: &[String],
    previously_verified: Sha256Digest, emergency_serial_floor: u64, now: u64) -> Result<EmergencyAuthorization> {
    validate_root(root, now)?;
    let authorization: EmergencyAuthorization = decode_canonical(bytes)?;
    if authorization.schema_version != 1 || authorization.previously_signed_manifest != previously_verified {
        return Err(Error::Signature);
    }
    if authorization.expires_at_unix <= now { return Err(Error::Expired); }
    if authorization.serial <= emergency_serial_floor { return Err(Error::Rollback); }
    let signatures = signatures(armors)?;
    reject_unknown(&signatures, &[root])?;
    threshold(root, &signatures, EMERGENCY_SSHSIG_NAMESPACE, bytes)?;
    Ok(authorization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metadata::canonical_bytes, sshsig::test_support::{key, sign}};
    use ed25519_dalek::SigningKey as PrivateKey;

    fn root(seed: u8) -> (RootMetadata, Vec<PrivateKey>, PrivateKey) {
        let pairs = [key(seed, Some(ROOT_SK_APPLICATION)), key(seed + 1, None), key(seed + 2, None)];
        let (target_secret, target_key) = key(seed + 3, Some(TARGET_SK_APPLICATION));
        (RootMetadata { schema_version: 1, version: 1, expires_at_unix: 1000,
            root_threshold: 2, root_keys: pairs.iter().map(|(_, key)| key.clone()).collect(),
            target_key, revoked_key_ids: vec![], frozen: false },
            pairs.into_iter().map(|(secret, _)| secret).collect(), target_secret)
    }

    fn root_sigs(root: &RootMetadata, secrets: &[PrivateKey], bytes: &[u8], indices: &[usize]) -> Vec<String> {
        indices.iter().map(|index| sign(&secrets[*index], &root.root_keys[*index], ROOT_SSHSIG_NAMESPACE, bytes, 5)).collect()
    }

    fn manifest() -> ReleaseManifest {
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw,
            version: ReleaseVersion { major: 1, minor: 80, patch: 0 },
            minimum_version: ReleaseVersion { major: 1, minor: 79, patch: 3 },
            channel: ReleaseChannel::Stable, os: OperatingSystem::Linux, arch: Architecture::X86_64,
            serial: 2, created_at_unix: 100, expires_at_unix: 500,
            artifacts: vec![ReleaseArtifact { id: "gateway".into(), sha256: Sha256Digest([1; 32]),
                size_bytes: 4096, mode: 0o755, install_target: InstallTarget::Gateway }],
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1,
                updated: 1, mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] }
    }

    #[test]
    fn root_threshold_all_subsets_duplicate_identity_and_pin() {
        let (root, secrets, _) = root(1);
        let bytes = canonical_bytes(&root).unwrap();
        let digest = Sha256Digest(Sha256::digest(&bytes).into());
        for bits in 0u8..8 {
            let indices: Vec<_> = (0..3).filter(|index| bits & (1 << index) != 0).collect();
            let sigs = root_sigs(&root, &secrets, &bytes, &indices);
            assert_eq!(verify_root(&bytes, &sigs, digest, 100).is_ok(), bits.count_ones() >= 2);
        }
        let duplicate = root_sigs(&root, &secrets, &bytes, &[1, 1, 2]);
        assert!(verify_root(&bytes, &duplicate, digest, 100).is_err());
        let sigs = root_sigs(&root, &secrets, &bytes, &[1, 2]);
        assert!(verify_root(&bytes, &sigs, Sha256Digest([0; 32]), 100).is_err());
        assert!(verify_root(&bytes, &sigs, digest, 1000).is_err());
        let mut alias = root; alias.root_keys[1].key_id = Sha256Digest([9; 32]);
        assert!(validate_root(&alias, 100).is_err());
    }

    #[test]
    fn rotation_requires_both_thresholds_adjacent_version_and_permanent_revocations() {
        let (old, old_secrets, _) = root(1);
        let (mut new, new_secrets, _) = root(10); new.version = 2;
        let bytes = canonical_bytes(&new).unwrap();
        let old_sigs = root_sigs(&old, &old_secrets, &bytes, &[1, 2]);
        let new_sigs = root_sigs(&new, &new_secrets, &bytes, &[1, 2]);
        assert!(rotate_root(&old, &bytes, &old_sigs, 100).is_err());
        assert!(rotate_root(&old, &bytes, &new_sigs, 100).is_err());
        let both: Vec<_> = old_sigs.into_iter().chain(new_sigs).collect();
        assert_eq!(rotate_root(&old, &bytes, &both, 100).unwrap(), new);
        new.version = 3;
        assert!(rotate_root(&old, &canonical_bytes(&new).unwrap(), &both, 100).is_err());
        let mut revoked = old; revoked.revoked_key_ids.push(Sha256Digest([99; 32]));
        new.version = 2;
        assert!(rotate_root(&revoked, &canonical_bytes(&new).unwrap(), &both, 100).is_err());
    }

    #[test]
    fn release_roles_expiry_freeze_revocation_and_signature_matrix() {
        let (root, secrets, target) = root(1);
        let bytes = canonical_bytes(&manifest()).unwrap();
        let signature = sign(&target, &root.target_key, RELEASE_SSHSIG_NAMESPACE, &bytes, 5);
        for now in [100, 101, 499] { assert!(verify_release(&root, &bytes, &signature, now).is_ok()); }
        for now in [0, 99, 500, 1000] { assert!(verify_release(&root, &bytes, &signature, now).is_err()); }
        let wrong_role = sign(&secrets[1], &root.root_keys[1], RELEASE_SSHSIG_NAMESPACE, &bytes, 5);
        assert!(verify_release(&root, &bytes, &wrong_role, 100).is_err());
        let wrong_namespace = sign(&target, &root.target_key, ROOT_SSHSIG_NAMESPACE, &bytes, 5);
        assert!(verify_release(&root, &bytes, &wrong_namespace, 100).is_err());
        let mut frozen = root.clone(); frozen.frozen = true;
        assert!(verify_release(&frozen, &bytes, &signature, 100).is_err());
        assert!(verify_installed_release(&frozen, &bytes, &signature, 100, 600).is_ok());
        assert!(verify_installed_release(&frozen, &bytes, &signature, 500, 600).is_err());
        assert!(verify_installed_release(&frozen, &bytes, &signature, 100, 1000).is_err());
        assert!(verify_installed_release(&frozen, &bytes, &signature, 100, 99).is_err());
        let mut revoked = root; revoked.revoked_key_ids.push(revoked.target_key.key_id);
        assert!(verify_release(&revoked, &bytes, &signature, 100).is_err());
    }

    #[test]
    fn emergency_is_exact_prior_digest_expiring_and_serial_fenced() {
        let (root, secrets, _) = root(1);
        let digest = Sha256Digest([42; 32]);
        let authorization = EmergencyAuthorization { schema_version: 1, serial: 3,
            previously_signed_manifest: digest, expires_at_unix: 200 };
        let bytes = canonical_bytes(&authorization).unwrap();
        let sigs: Vec<_> = [1, 2].iter().map(|index| sign(&secrets[*index], &root.root_keys[*index],
            EMERGENCY_SSHSIG_NAMESPACE, &bytes, 5)).collect();
        assert!(verify_emergency(&root, &bytes, &sigs, digest, 2, 100).is_ok());
        assert!(verify_emergency(&root, &bytes, &sigs, digest, 3, 100).is_err());
        assert!(verify_emergency(&root, &bytes, &sigs, digest, 2, 200).is_err());
        assert!(verify_emergency(&root, &bytes, &sigs, Sha256Digest([0; 32]), 2, 100).is_err());
        assert!(verify_emergency(&root, &bytes, &root_sigs(&root, &secrets, &bytes, &[1, 2]), digest, 2, 100).is_err());
    }
}
