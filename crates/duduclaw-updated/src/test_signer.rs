//! TEST-ONLY software signer for the isolation test root key set (plan §3.3,
//! R2 §4.2). Compiled only in unit tests and with feature `test-signer`; the
//! `duduclaw-isolation-test-signer` binary requires that feature and is never in
//! `scripts/package-linux-release.py` `BIN_PACKAGES` (test-key leak layer 1).
//!
//! It simulates what no ordinary `ssh-keygen` can produce without a device:
//! `sk-ssh-ed25519@openssh.com` signatures with the UP|UV flags a FIDO2
//! authenticator sets, so the production verifier (`sshsig::SshSignature::verify`)
//! accepts them. A software "security key" proves nothing about hardware, which is
//! exactly why these keys may only ever pin a `cfg(duduclaw_test_root)` build that
//! reports `TEST <pin>` (layer 2). This module holds no key material itself.
use base64::{engine::general_purpose::STANDARD, Engine};
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use ed25519_dalek::{Signer, SigningKey as PrivateKey};
use sha2::{Digest, Sha256, Sha512};

/// First line of every test key file. Key files of any other format (OpenSSH
/// private keys, FIDO2 key handles, minisign secret keys) are refused.
pub const KEY_FILE_HEADER: &str = "duduclaw-isolation-TEST-ONLY-key-v1";
/// Counter carried by simulated security-key signatures (the verifier only records it).
pub const SIMULATED_COUNTER: u32 = 42;

const ED25519: &[u8] = b"ssh-ed25519";
const SK_ED25519: &[u8] = b"sk-ssh-ed25519@openssh.com";

/// Role of a test key; fixes its algorithm and security-key application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// R1: simulated `sk-ssh-ed25519`, application `ssh:duduclaw-root`.
    RootSecurityKey,
    /// R2/R3: plain `ssh-ed25519`.
    Root,
    /// Target: simulated `sk-ssh-ed25519`, application `ssh:duduclaw-target`.
    TargetSecurityKey,
    /// Legacy release minisign key of a test-root verifier build.
    Minisign,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::RootSecurityKey, Role::Root, Role::TargetSecurityKey, Role::Minisign];

    pub fn name(self) -> &'static str {
        match self {
            Self::RootSecurityKey => "root-sk",
            Self::Root => "root",
            Self::TargetSecurityKey => "target-sk",
            Self::Minisign => "minisign",
        }
    }

    pub fn parse(text: &str) -> Option<Self> { Self::ALL.into_iter().find(|role| role.name() == text) }

    /// `None` for a plain ed25519 key; the fixed role application for a security key.
    pub fn application(self) -> Option<&'static str> {
        match self {
            Self::RootSecurityKey => Some(ROOT_SK_APPLICATION),
            Self::TargetSecurityKey => Some(TARGET_SK_APPLICATION),
            Self::Root | Self::Minisign => None,
        }
    }

    /// Root keys sign root metadata, emergency and migration-bundle documents;
    /// only the target key signs release/runtime manifests.
    pub fn may_sign(self, namespace: &str) -> bool {
        match self {
            Self::RootSecurityKey | Self::Root => [ROOT_SSHSIG_NAMESPACE, EMERGENCY_SSHSIG_NAMESPACE,
                MIGRATION_BUNDLE_SSHSIG_NAMESPACE].contains(&namespace),
            Self::TargetSecurityKey => [RELEASE_SSHSIG_NAMESPACE, RUNTIME_SSHSIG_NAMESPACE].contains(&namespace),
            Self::Minisign => false,
        }
    }
}

fn ssh_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

/// Public `SigningKey` exactly as root metadata registers it.
pub fn signing_key(secret: &PrivateKey, application: Option<&str>) -> SigningKey {
    let mut blob = Vec::new();
    ssh_string(&mut blob, if application.is_some() { SK_ED25519 } else { ED25519 });
    ssh_string(&mut blob, &secret.verifying_key().to_bytes());
    if let Some(application) = application { ssh_string(&mut blob, application.as_bytes()); }
    SigningKey { key_id: Sha256Digest(Sha256::digest(&blob).into()),
        algorithm: if application.is_some() { SigningAlgorithm::SkEd25519 } else { SigningAlgorithm::Ed25519 },
        public_blob: blob, application: application.map(str::to_owned) }
}

/// Deterministic key from a one-byte seed (unit-test fixtures only).
pub fn key(seed: u8, application: Option<&str>) -> (PrivateKey, SigningKey) {
    let secret = PrivateKey::from_bytes(&[seed; 32]);
    let public = signing_key(&secret, application);
    (secret, public)
}

/// PEM armor as `ssh-keygen -Y sign` writes it (70-column base64 lines).
pub fn armor(bytes: &[u8]) -> String {
    let encoded = STANDARD.encode(bytes);
    let lines: Vec<&str> = encoded.as_bytes().chunks(70)
        .map(|line| std::str::from_utf8(line).unwrap_or_default()).collect();
    format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", lines.join("\n"))
}

/// SSHSIG (OpenSSH PROTOCOL.sshsig, sha512). For a security key the signed data
/// is the PROTOCOL.u2f Ed25519 structure with `flags` and [`SIMULATED_COUNTER`].
pub fn sign(secret: &PrivateKey, key: &SigningKey, namespace: &str, message: &[u8], flags: u8) -> String {
    let mut signed = b"SSHSIG".to_vec();
    ssh_string(&mut signed, namespace.as_bytes());
    ssh_string(&mut signed, b"");
    ssh_string(&mut signed, SSHSIG_HASH_ALGORITHM.as_bytes());
    ssh_string(&mut signed, &Sha512::digest(message));
    let mut sigblob = Vec::new();
    if let Some(application) = key.application.as_deref() {
        let mut authenticator = Sha256::digest(application.as_bytes()).to_vec();
        authenticator.push(flags);
        authenticator.extend_from_slice(&SIMULATED_COUNTER.to_be_bytes());
        authenticator.extend_from_slice(&Sha256::digest(signed));
        ssh_string(&mut sigblob, SK_ED25519);
        ssh_string(&mut sigblob, &secret.sign(&authenticator).to_bytes());
        sigblob.push(flags);
        sigblob.extend_from_slice(&SIMULATED_COUNTER.to_be_bytes());
    } else {
        ssh_string(&mut sigblob, ED25519);
        ssh_string(&mut sigblob, &secret.sign(&signed).to_bytes());
    }
    let mut envelope = b"SSHSIG".to_vec();
    envelope.extend_from_slice(&SSHSIG_VERSION.to_be_bytes());
    ssh_string(&mut envelope, &key.public_blob);
    ssh_string(&mut envelope, namespace.as_bytes());
    ssh_string(&mut envelope, b"");
    ssh_string(&mut envelope, SSHSIG_HASH_ALGORITHM.as_bytes());
    ssh_string(&mut envelope, &sigblob);
    armor(&envelope)
}

/// One OpenSSH public-key line (`<algorithm> <base64 blob> <comment>`), the input
/// format of `scripts/isolation-root-metadata.py`.
pub fn openssh_public_line(key: &SigningKey, comment: &str) -> String {
    let algorithm = if key.application.is_some() { "sk-ssh-ed25519@openssh.com" } else { "ssh-ed25519" };
    format!("{algorithm} {} {comment}\n", STANDARD.encode(&key.public_blob))
}

/// A test key file: role plus 32-byte ed25519 seed. Never a production format.
pub struct TestKey { pub role: Role, pub secret: PrivateKey }

impl TestKey {
    pub fn from_seed(role: Role, seed: [u8; 32]) -> Self { Self { role, secret: PrivateKey::from_bytes(&seed) } }

    /// `header\nrole <name>\nseed <64 hex>\n`.
    pub fn to_file(&self) -> String {
        format!("{KEY_FILE_HEADER}\nrole {}\nseed {}\n", self.role.name(), hex::encode(self.secret.to_bytes()))
    }

    /// Strict inverse of [`TestKey::to_file`]; anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let body = text.strip_suffix('\n')?;
        let mut lines = body.split('\n');
        if lines.next()? != KEY_FILE_HEADER { return None; }
        let role = Role::parse(lines.next()?.strip_prefix("role ")?)?;
        let seed = lines.next()?.strip_prefix("seed ")?;
        if lines.next().is_some() || seed.len() != 64
            || !seed.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
        let bytes: [u8; 32] = hex::decode(seed).ok()?.try_into().ok()?;
        Some(Self::from_seed(role, bytes))
    }

    /// The registered public key; `None` for the minisign role.
    pub fn public(&self) -> Option<SigningKey> {
        match self.role {
            Role::Minisign => None,
            role => Some(signing_key(&self.secret, role.application())),
        }
    }

    /// SSHSIG under `namespace`; security keys set UP|UV like a real authenticator.
    pub fn sign(&self, namespace: &str, message: &[u8]) -> Option<String> {
        if !self.role.may_sign(namespace) { return None; }
        let public = self.public()?;
        Some(sign(&self.secret, &public, namespace, message, REQUIRED_SK_FLAGS))
    }

    pub fn minisign(&self) -> Option<TestMinisign> {
        (self.role == Role::Minisign).then(|| TestMinisign::new(self.secret.clone()))
    }
}

/// Minisign test key (prehashed `ED` signatures only, as the verifier requires).
pub struct TestMinisign { secret: PrivateKey, key_id: [u8; 8] }

impl TestMinisign {
    pub fn new(secret: PrivateKey) -> Self {
        let digest = Sha256::digest(secret.verifying_key().to_bytes());
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(digest.get(..8).unwrap_or(&[0u8; 8]));
        Self { secret, key_id }
    }

    /// Base64 public key (`DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY` of a test-root build).
    pub fn public(&self) -> String {
        let mut blob = b"Ed".to_vec();
        blob.extend_from_slice(&self.key_id);
        blob.extend_from_slice(&self.secret.verifying_key().to_bytes());
        STANDARD.encode(blob)
    }

    /// `minisign -P`-compatible public key file.
    pub fn public_file(&self) -> String {
        format!("untrusted comment: duduclaw TEST-ONLY minisign public key {}\n{}\n",
            hex::encode_upper(self.key_id), self.public())
    }

    /// Signature over a BLAKE2b-512 prehash. `None` when the trusted comment is not
    /// one printable line (the verifier's strict text rule would refuse it).
    pub fn sign_prehash(&self, blake2b512: &[u8], trusted_comment: &str) -> Option<String> {
        if blake2b512.len() != 64 || trusted_comment.is_empty()
            || !trusted_comment.chars().all(|c| !c.is_control()) { return None; }
        let signature = self.secret.sign(blake2b512);
        let mut blob = b"ED".to_vec();
        blob.extend_from_slice(&self.key_id);
        blob.extend_from_slice(&signature.to_bytes());
        let mut global = signature.to_bytes().to_vec();
        global.extend_from_slice(trusted_comment.as_bytes());
        let global = self.secret.sign(&global);
        Some(format!("untrusted comment: duduclaw TEST-ONLY signature\n{}\ntrusted comment: {trusted_comment}\n{}\n",
            STANDARD.encode(blob), STANDARD.encode(global.to_bytes())))
    }

    pub fn sign(&self, data: &[u8], trusted_comment: &str) -> Option<String> {
        // `Digest` (digest 0.10) is already in scope through sha2.
        self.sign_prehash(blake2::Blake2b512::digest(data).as_slice(), trusted_comment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metadata::canonical_bytes, Error};

    const NOW: u64 = 1_800_000_000;
    const EXPIRES: u64 = 4_000_000_000;

    /// The key set scripts/isolation-test-keys.sh creates, from fixed seeds.
    fn key_set() -> (Vec<TestKey>, TestKey, TestKey) {
        let roots = vec![TestKey::from_seed(Role::RootSecurityKey, [31; 32]), TestKey::from_seed(Role::Root, [32; 32]),
            TestKey::from_seed(Role::Root, [33; 32])];
        (roots, TestKey::from_seed(Role::TargetSecurityKey, [34; 32]), TestKey::from_seed(Role::Minisign, [35; 32]))
    }

    fn root_metadata(roots: &[TestKey], target: &TestKey) -> RootMetadata {
        RootMetadata { schema_version: 1, version: 1, expires_at_unix: EXPIRES, root_threshold: 2,
            root_keys: roots.iter().map(|key| key.public().unwrap()).collect(), target_key: target.public().unwrap(),
            revoked_key_ids: vec![], frozen: false }
    }

    fn manifest() -> ReleaseManifest {
        ReleaseManifest { schema_version: 1, product: Product::Duduclaw,
            version: ReleaseVersion { major: 1, minor: 83, patch: 0 }, channel: ReleaseChannel::Stable,
            os: OperatingSystem::Linux, arch: Architecture::X86_64,
            minimum_version: ReleaseVersion { major: 1, minor: 82, patch: 0 }, serial: 1_083_000,
            created_at_unix: NOW - 10, expires_at_unix: EXPIRES,
            artifacts: vec![ReleaseArtifact { id: "duduclaw".into(), sha256: Sha256Digest([1; 32]), size_bytes: 4096,
                mode: 0o755, install_target: InstallTarget::Gateway }],
            protocols: ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1,
                mcp_bridge: 1, parse_worker: 1, importer: 1 }, migrations: vec![] }
    }

    #[test]
    fn signatures_verify_with_production_verifier() {
        let (roots, target, minisign) = key_set();
        let root = root_metadata(&roots, &target);
        let root_bytes = canonical_bytes(&root).unwrap();
        let pin = Sha256Digest(Sha256::digest(&root_bytes).into());
        // root.json: R1 (simulated security key) + R2, exactly as the kit signs it.
        let root_signatures: Vec<String> = [&roots[0], &roots[1]].iter()
            .map(|key| key.sign(ROOT_SSHSIG_NAMESPACE, &root_bytes).unwrap()).collect();
        let verified = crate::trust::verify_root(&root_bytes, &root_signatures, pin, NOW).unwrap();
        assert_eq!(verified, root);
        // Release manifest: target (simulated security key).
        let manifest_bytes = canonical_bytes(&manifest()).unwrap();
        let manifest_signature = target.sign(RELEASE_SSHSIG_NAMESPACE, &manifest_bytes).unwrap();
        assert_eq!(crate::trust::verify_release(&root, &manifest_bytes, &manifest_signature, NOW).unwrap(), manifest());
        // BundleAuthorization: R1 + R3 under the migration-bundle namespace.
        let image = b"bundle image bytes";
        let authorization = BundleAuthorization { schema_version: 1, product: Product::Duduclaw,
            version: ReleaseVersion { major: 1, minor: 83, patch: 0 }, arch: Architecture::Aarch64,
            bundle_sha256: Sha256Digest(Sha256::digest(image).into()), bundle_size: image.len() as u64,
            expires_at_unix: EXPIRES };
        let authorization_bytes = canonical_bytes(&authorization).unwrap();
        let authorization_signatures: Vec<String> = [&roots[0], &roots[2]].iter()
            .map(|key| key.sign(MIGRATION_BUNDLE_SSHSIG_NAMESPACE, &authorization_bytes).unwrap()).collect();
        assert_eq!(crate::bundle::verify_authorization(&root, &authorization_bytes, &authorization_signatures,
            authorization.bundle_sha256, authorization.bundle_size, Architecture::Aarch64, authorization.version, NOW)
            .unwrap(), authorization);
        // One root signature is below the threshold; the target is not a root key.
        assert!(crate::bundle::verify_authorization(&root, &authorization_bytes, &authorization_signatures[..1],
            authorization.bundle_sha256, authorization.bundle_size, Architecture::Aarch64, authorization.version, NOW).is_err());
        // Role/namespace separation is enforced by the signer as well as the verifier.
        assert!(target.sign(MIGRATION_BUNDLE_SSHSIG_NAMESPACE, &authorization_bytes).is_none());
        assert!(roots[1].sign(RELEASE_SSHSIG_NAMESPACE, &manifest_bytes).is_none());
        assert!(minisign.sign(ROOT_SSHSIG_NAMESPACE, &root_bytes).is_none());
        let forged = sign(&target.secret, &target.public().unwrap(), RELEASE_SSHSIG_NAMESPACE, &manifest_bytes, 0x01);
        assert!(matches!(crate::trust::verify_release(&root, &manifest_bytes, &forged, NOW), Err(Error::Signature)));
        // Legacy half: prehashed minisign with the exact trusted comment.
        let signer = minisign.minisign().unwrap();
        let comment = "duduclaw v1.83.0 duduclaw-isolated-linux-aarch64-1.83.0.tar";
        let minisig = signer.sign(image, comment).unwrap();
        crate::bundle::verify_legacy_signature(image, &minisig, &signer.public(), comment).unwrap();
        assert!(crate::bundle::verify_legacy_signature(b"other", &minisig, &signer.public(), comment).is_err());
        assert!(crate::bundle::verify_legacy_signature(image, &minisig, LEGACY_RELEASE_MINISIGN_KEY, comment).is_err());
        assert!(signer.sign(image, "two\nlines").is_none());
        assert!(roots[0].minisign().is_none());
        // The public-key line round-trips to the registered blob.
        for key in roots.iter().chain(std::iter::once(&target)) {
            let public = key.public().unwrap();
            let line = openssh_public_line(&public, "duduclaw-TEST-ONLY");
            let blob = STANDARD.decode(line.split(' ').nth(1).unwrap()).unwrap();
            assert_eq!(blob, public.public_blob);
            crate::sshsig::validate_key(&public).unwrap();
        }
    }

    #[test]
    fn key_files_round_trip_and_refuse_other_formats() {
        for role in Role::ALL {
            let key = TestKey::from_seed(role, [7; 32]);
            let text = key.to_file();
            assert!(text.starts_with(KEY_FILE_HEADER));
            let parsed = TestKey::parse(&text).unwrap();
            assert_eq!(parsed.role, role);
            assert_eq!(parsed.secret.to_bytes(), [7; 32]);
            assert_eq!(Role::parse(role.name()), Some(role));
        }
        let good = TestKey::from_seed(Role::Root, [7; 32]).to_file();
        for bad in [String::new(), good.trim_end().to_owned(), format!("{good}\n"), good.replace("role root", "role admin"),
            good.replace(KEY_FILE_HEADER, "duduclaw-isolation-key-v1"), good.to_uppercase(), good.replace("seed 07", "seed 0g"),
            "-----BEGIN OPENSSH PRIVATE KEY-----\n".to_owned(), good.replace('\n', "\r\n")] {
            assert!(TestKey::parse(&bad).is_none(), "{bad:?}");
        }
    }
}
