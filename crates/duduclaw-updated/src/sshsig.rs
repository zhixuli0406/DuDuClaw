//! Strict OpenSSH SSHSIG verification. No subprocess or private-key parsing.
//! Wire layouts: OpenSSH PROTOCOL.sshsig and PROTOCOL.u2f (Ed25519 section).
use base64::{engine::general_purpose::STANDARD, Engine};
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};

use crate::{Error, Result};

const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
const ED25519: &[u8] = b"ssh-ed25519";
const SK_ED25519: &[u8] = b"sk-ssh-ed25519@openssh.com";

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let bytes = self.0.get(..length).ok_or(Error::Signature)?;
        self.0 = self.0.get(length..).ok_or(Error::Signature)?;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| Error::Signature)?))
    }
    fn string(&mut self) -> Result<&'a [u8]> {
        let length = self.u32()? as usize;
        self.take(length)
    }
    fn end(&self) -> Result<()> {
        if self.0.is_empty() { Ok(()) } else { Err(Error::Signature) }
    }
}

fn ssh_string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

fn unarmor(armor: &str) -> Result<Vec<u8>> {
    if armor.len() > MAX_SIGNATURE_BYTES { return Err(Error::Signature); }
    let content = armor.strip_suffix('\n').unwrap_or(armor);
    let mut lines = content.split('\n');
    if lines.next() != Some("-----BEGIN SSH SIGNATURE-----") { return Err(Error::Signature); }
    let mut encoded = String::new();
    loop {
        let line = lines.next().ok_or(Error::Signature)?;
        if line == "-----END SSH SIGNATURE-----" {
            if lines.next().is_some() || encoded.is_empty() { return Err(Error::Signature); }
            break;
        }
        if line.is_empty() || line.len() > 76
            || !line.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        { return Err(Error::Signature); }
        encoded.push_str(line);
    }
    let bytes = STANDARD.decode(&encoded).map_err(|_| Error::Signature)?;
    if STANDARD.encode(&bytes) != encoded { return Err(Error::Signature); }
    Ok(bytes)
}

pub struct SshSignature {
    public_blob: Vec<u8>,
    namespace: Vec<u8>,
    signature: Vec<u8>,
}

impl SshSignature {
    pub fn parse(armor: &str) -> Result<Self> {
        let bytes = unarmor(armor)?;
        let mut reader = Reader(&bytes);
        if reader.take(6)? != b"SSHSIG" || reader.u32()? != SSHSIG_VERSION { return Err(Error::Signature); }
        let public_blob = reader.string()?.to_vec();
        if public_blob.is_empty() || public_blob.len() > MAX_KEY_BLOB_BYTES { return Err(Error::Signature); }
        let namespace = reader.string()?.to_vec();
        if namespace.is_empty() || namespace.len() > 128 || !reader.string()?.is_empty()
            || reader.string()? != SSHSIG_HASH_ALGORITHM.as_bytes()
        { return Err(Error::Signature); }
        let signature = reader.string()?.to_vec();
        reader.end()?;
        Ok(Self { public_blob, namespace, signature })
    }

    pub fn key_id(&self) -> Sha256Digest { Sha256Digest(Sha256::digest(&self.public_blob).into()) }

    /// Returns the authenticated FIDO counter for diagnostics only, never freshness.
    pub fn verify(&self, key: &SigningKey, namespace: &str, message: &[u8]) -> Result<Option<u32>> {
        if self.namespace != namespace.as_bytes() || self.public_blob != key.public_blob {
            return Err(Error::Signature);
        }
        let public = validate_key(key)?;
        let mut signed = b"SSHSIG".to_vec();
        ssh_string(&mut signed, namespace.as_bytes());
        ssh_string(&mut signed, b"");
        ssh_string(&mut signed, SSHSIG_HASH_ALGORITHM.as_bytes());
        ssh_string(&mut signed, &Sha512::digest(message));
        let mut reader = Reader(&self.signature);
        let algorithm = reader.string()?;
        let signature = Signature::from_slice(reader.string()?).map_err(|_| Error::Signature)?;
        let counter = match key.algorithm {
            SigningAlgorithm::Ed25519 if algorithm == ED25519 => None,
            SigningAlgorithm::SkEd25519 if algorithm == SK_ED25519 => {
                let flags = reader.take(1)?[0];
                // No extension encoding is supported. UP and UV are both mandatory.
                if flags != REQUIRED_SK_FLAGS { return Err(Error::Signature); }
                let counter = reader.u32()?;
                let application = key.application.as_deref().ok_or(Error::Signature)?;
                let mut authenticator = Sha256::digest(application.as_bytes()).to_vec();
                authenticator.push(flags);
                authenticator.extend_from_slice(&counter.to_be_bytes());
                authenticator.extend_from_slice(&Sha256::digest(&signed));
                signed = authenticator;
                Some(counter)
            }
            _ => return Err(Error::Signature),
        };
        reader.end()?;
        public.verify_strict(&signed, &signature).map_err(|_| Error::Signature)?;
        Ok(counter)
    }
}

/// Check blob identity, algorithm, key shape and exact role-specific application.
/// ECDSA security keys are deliberately unsupported until independently tested.
pub fn validate_key(key: &SigningKey) -> Result<VerifyingKey> {
    if key.public_blob.is_empty() || key.public_blob.len() > MAX_KEY_BLOB_BYTES
        || Sha256Digest(Sha256::digest(&key.public_blob).into()) != key.key_id
    { return Err(Error::Signature); }
    let mut reader = Reader(&key.public_blob);
    let algorithm = reader.string()?;
    let raw: [u8; 32] = reader.string()?.try_into().map_err(|_| Error::Signature)?;
    match key.algorithm {
        SigningAlgorithm::Ed25519 if algorithm == ED25519 && key.application.is_none() => {}
        SigningAlgorithm::SkEd25519 if algorithm == SK_ED25519 => {
            let application = key.application.as_deref().ok_or(Error::Signature)?;
            if ![ROOT_SK_APPLICATION, TARGET_SK_APPLICATION].iter().any(|value| *value == application)
                || reader.string()? != application.as_bytes()
            { return Err(Error::Signature); }
        }
        _ => return Err(Error::Incompatible),
    }
    reader.end()?;
    let public = VerifyingKey::from_bytes(&raw).map_err(|_| Error::Signature)?;
    if public.is_weak() { return Err(Error::Signature); }
    Ok(public)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey as PrivateKey};

    pub fn key(seed: u8, application: Option<&str>) -> (PrivateKey, SigningKey) {
        let secret = PrivateKey::from_bytes(&[seed; 32]);
        let mut blob = Vec::new();
        ssh_string(&mut blob, if application.is_some() { SK_ED25519 } else { ED25519 });
        ssh_string(&mut blob, &secret.verifying_key().to_bytes());
        if let Some(application) = application { ssh_string(&mut blob, application.as_bytes()); }
        (secret, SigningKey { key_id: Sha256Digest(Sha256::digest(&blob).into()),
            algorithm: if application.is_some() { SigningAlgorithm::SkEd25519 } else { SigningAlgorithm::Ed25519 },
            public_blob: blob, application: application.map(str::to_owned) })
    }

    pub fn armor(bytes: &[u8]) -> String {
        let encoded = STANDARD.encode(bytes);
        let lines: Vec<_> = encoded.as_bytes().chunks(70)
            .map(|line| std::str::from_utf8(line).unwrap()).collect();
        format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", lines.join("\n"))
    }

    pub fn sign(secret: &PrivateKey, key: &SigningKey, namespace: &str, message: &[u8], flags: u8) -> String {
        let mut signed = b"SSHSIG".to_vec();
        ssh_string(&mut signed, namespace.as_bytes());
        ssh_string(&mut signed, b"");
        ssh_string(&mut signed, b"sha512");
        ssh_string(&mut signed, &Sha512::digest(message));
        let mut sigblob = Vec::new();
        if let Some(application) = key.application.as_deref() {
            let mut auth = Sha256::digest(application.as_bytes()).to_vec();
            auth.push(flags);
            auth.extend_from_slice(&42u32.to_be_bytes());
            auth.extend_from_slice(&Sha256::digest(signed));
            ssh_string(&mut sigblob, SK_ED25519);
            ssh_string(&mut sigblob, &secret.sign(&auth).to_bytes());
            sigblob.push(flags);
            sigblob.extend_from_slice(&42u32.to_be_bytes());
        } else {
            ssh_string(&mut sigblob, ED25519);
            ssh_string(&mut sigblob, &secret.sign(&signed).to_bytes());
        }
        let mut envelope = b"SSHSIG".to_vec();
        envelope.extend_from_slice(&1u32.to_be_bytes());
        ssh_string(&mut envelope, &key.public_blob);
        ssh_string(&mut envelope, namespace.as_bytes());
        ssh_string(&mut envelope, b"");
        ssh_string(&mut envelope, b"sha512");
        ssh_string(&mut envelope, &sigblob);
        armor(&envelope)
    }

    pub fn fixture_key() -> SigningKey {
        let public = include_str!("../tests/fixtures/ed25519-1.pub");
        let blob = STANDARD.decode(public.split_whitespace().nth(1).unwrap()).unwrap();
        SigningKey { key_id: Sha256Digest(Sha256::digest(&blob).into()), algorithm: SigningAlgorithm::Ed25519,
            public_blob: blob, application: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::*;

    const MESSAGE: &[u8] = include_bytes!("../tests/fixtures/message.json");
    const ROOT_SIG: &str = include_str!("../tests/fixtures/ed25519-1-root.sig");

    #[test]
    fn openssh_generated_ed25519_vectors() {
        let key = fixture_key();
        for (signature, namespace) in [(ROOT_SIG, ROOT_SSHSIG_NAMESPACE),
            (include_str!("../tests/fixtures/ed25519-1-release.sig"), RELEASE_SSHSIG_NAMESPACE)] {
            assert_eq!(SshSignature::parse(signature).unwrap().verify(&key, namespace, MESSAGE).unwrap(), None);
            assert!(SshSignature::parse(signature).unwrap().verify(&key, namespace, b"changed").is_err());
        }
        assert!(SshSignature::parse(ROOT_SIG).unwrap().verify(&key, RELEASE_SSHSIG_NAMESPACE, MESSAGE).is_err());
        assert!(SshSignature::parse(include_str!("../tests/fixtures/ed25519-1-wrong.sig")).unwrap()
            .verify(&key, ROOT_SSHSIG_NAMESPACE, MESSAGE).is_err());
        assert!(SshSignature::parse(include_str!("../tests/fixtures/ed25519-2-root.sig")).unwrap()
            .verify(&key, ROOT_SSHSIG_NAMESPACE, MESSAGE).is_err());
        let blob = STANDARD.decode(include_str!("../tests/fixtures/ed25519-2.pub")
            .split_whitespace().nth(1).unwrap()).unwrap();
        let second_key = SigningKey { key_id: Sha256Digest(Sha256::digest(&blob).into()),
            algorithm: SigningAlgorithm::Ed25519, public_blob: blob, application: None };
        for (signature, namespace) in [(include_str!("../tests/fixtures/ed25519-2-root.sig"), ROOT_SSHSIG_NAMESPACE),
            (include_str!("../tests/fixtures/ed25519-2-release.sig"), RELEASE_SSHSIG_NAMESPACE)] {
            assert!(SshSignature::parse(signature).unwrap().verify(&second_key, namespace, MESSAGE).is_ok());
        }
        assert!(SshSignature::parse(include_str!("../tests/fixtures/ed25519-2-wrong.sig")).unwrap()
            .verify(&second_key, ROOT_SSHSIG_NAMESPACE, MESSAGE).is_err());
    }

    #[test]
    fn sshsig_rejects_all_truncations_trailing_data_and_noncanonical_armor() {
        let bytes = unarmor(ROOT_SIG).unwrap();
        for end in 0..bytes.len() { assert!(SshSignature::parse(&armor(&bytes[..end])).is_err()); }
        let mut trailing = bytes.clone(); trailing.push(0);
        assert!(SshSignature::parse(&armor(&trailing)).is_err());
        let mut version = bytes; version[9] = 2;
        assert!(SshSignature::parse(&armor(&version)).is_err());
        for bad in [format!(" {ROOT_SIG}"), format!("{ROOT_SIG}\n"), format!("{ROOT_SIG}garbage"),
            format!("{ROOT_SIG}{ROOT_SIG}"), ROOT_SIG.replace('\n', "\r\n"), ROOT_SIG.replace("U1NI", "U1 N I")] {
            assert!(SshSignature::parse(&bad).is_err());
        }
    }

    #[test]
    fn openssh_vector_rejects_each_corrupted_byte() {
        let original = unarmor(ROOT_SIG).unwrap();
        let key = fixture_key();
        for index in 0..original.len() {
            let mut changed = original.clone(); changed[index] ^= 1;
            assert!(SshSignature::parse(&armor(&changed)).and_then(|signature|
                signature.verify(&key, ROOT_SSHSIG_NAMESPACE, MESSAGE)).is_err(), "byte {index}");
        }
        let padded = STANDARD.encode(&original);
        if let Some(index) = padded.as_bytes().iter().position(|byte| *byte == b'=') {
            let mut noncanonical = padded.into_bytes();
            // Change unused padding bits without changing the decoded data.
            let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let previous = alphabet.iter().position(|byte| *byte == noncanonical[index - 1]).unwrap();
            noncanonical[index - 1] = alphabet[previous | 1];
            let armor = format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
                noncanonical.chunks(70).map(|line| std::str::from_utf8(line).unwrap()).collect::<Vec<_>>().join("\n"));
            assert!(SshSignature::parse(&armor).is_err());
        }
    }

    #[test]
    fn sshsig_rejects_reserved_hash_empty_namespace_and_key_aliases() {
        let bytes = unarmor(ROOT_SIG).unwrap();
        let mut reader = Reader(&bytes);
        reader.take(10).unwrap();
        let public = reader.string().unwrap();
        reader.string().unwrap(); reader.string().unwrap(); reader.string().unwrap();
        let signature = reader.string().unwrap();
        for (namespace, reserved, hash) in [(b"".as_slice(), b"".as_slice(), b"sha512".as_slice()),
            (ROOT_SSHSIG_NAMESPACE.as_bytes(), b"x", b"sha512"),
            (ROOT_SSHSIG_NAMESPACE.as_bytes(), b"", b"sha256")] {
            let mut bad = b"SSHSIG".to_vec(); bad.extend_from_slice(&1u32.to_be_bytes());
            for part in [public, namespace, reserved, hash, signature] { ssh_string(&mut bad, part); }
            assert!(SshSignature::parse(&armor(&bad)).is_err());
        }
        let mut key = fixture_key(); key.key_id.0[0] ^= 1;
        assert!(validate_key(&key).is_err());
        let mut key = fixture_key(); key.public_blob.push(0);
        key.key_id = Sha256Digest(Sha256::digest(&key.public_blob).into());
        assert!(validate_key(&key).is_err());
    }

    #[test]
    fn synthetic_security_key_flags_application_and_counter() {
        let (secret, key) = key(3, Some(TARGET_SK_APPLICATION));
        for flags in 0..=255 {
            let signature = sign(&secret, &key, RELEASE_SSHSIG_NAMESPACE, MESSAGE, flags);
            assert_eq!(SshSignature::parse(&signature).unwrap().verify(&key, RELEASE_SSHSIG_NAMESPACE, MESSAGE).is_ok(), flags == 5);
        }
        let signature = sign(&secret, &key, RELEASE_SSHSIG_NAMESPACE, MESSAGE, 5);
        assert_eq!(SshSignature::parse(&signature).unwrap().verify(&key, RELEASE_SSHSIG_NAMESPACE, MESSAGE).unwrap(), Some(42));
        let mut wrong_role = key; wrong_role.application = Some(ROOT_SK_APPLICATION.into());
        assert!(SshSignature::parse(&signature).unwrap().verify(&wrong_role, RELEASE_SSHSIG_NAMESPACE, MESSAGE).is_err());
    }

    #[test]
    #[ignore = "Requires Homebrew OpenSSH and real FIDO2 fixtures; see tests/fixtures/README.md"]
    fn real_fido2_sk_ed25519_openssh_vector() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fido2");
        let public = std::fs::read_to_string(directory.join("target.pub")).expect("generate real FIDO2 fixture first");
        let blob = STANDARD.decode(public.split_whitespace().nth(1).unwrap()).unwrap();
        let key = SigningKey { key_id: Sha256Digest(Sha256::digest(&blob).into()),
            algorithm: SigningAlgorithm::SkEd25519, public_blob: blob, application: Some(TARGET_SK_APPLICATION.into()) };
        let signature = std::fs::read_to_string(directory.join("message.sig")).unwrap();
        let message = std::fs::read(directory.join("message.json")).unwrap();
        assert!(SshSignature::parse(&signature).unwrap().verify(&key, RELEASE_SSHSIG_NAMESPACE, &message).unwrap().is_some());
    }
}
