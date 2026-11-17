//! duduclaw-isolation-test-signer: TEST-ONLY offline signer for the isolation
//! test root key set (plan §3.3, R2 §4.2). Built only with
//! `cargo build -p duduclaw-updated --features test-signer --bin duduclaw-isolation-test-signer`
//! (driven by scripts/isolation-test-keys.sh); never part of a release.
//!
//! Keys are software keys in the `test_signer::KEY_FILE_HEADER` format, written
//! 0600 to a caller-chosen path and never overwritten. Any other key format (an
//! OpenSSH private key, a FIDO2 key handle, a minisign secret key) is refused, so
//! a production key can never be used through this tool. Secret bytes are never
//! printed; errors are fixed one-line messages.
use std::{fs::{File, OpenOptions}, io::{Read, Write}, path::Path, process::ExitCode};

use duduclaw_isolation::update::{RootMetadata, EMERGENCY_SSHSIG_NAMESPACE, MAX_MIGRATION_BUNDLE_BYTES,
    MIGRATION_BUNDLE_SSHSIG_NAMESPACE, RELEASE_SSHSIG_NAMESPACE, ROOT_SSHSIG_NAMESPACE, RUNTIME_SSHSIG_NAMESPACE};
use duduclaw_updated::{metadata::{decode_canonical, MAX_METADATA_BYTES}, sshsig::SshSignature,
    test_signer::{openssh_public_line, Role, TestKey}};

const USAGE: &str = "TEST-ONLY isolation signer (never use for a release)
usage:
  duduclaw-isolation-test-signer keygen <root-sk|root|target-sk|minisign> <key-path>
      writes <key-path> (0600) and <key-path>.pub; prints the public key
  duduclaw-isolation-test-signer sign <key-path> <root|release|runtime|emergency|bundle> <input> <output>
      SSHSIG (sha512) of <input> under the fixed namespace of that document kind
  duduclaw-isolation-test-signer minisign <key-path> <input> <trusted-comment> <output>
      prehashed minisign signature (BLAKE2b-512) with exactly that trusted comment
  duduclaw-isolation-test-signer minisign-public <key-path>
      prints the base64 public key for DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY
  duduclaw-isolation-test-signer verify <root.json> <root|release|runtime|emergency|bundle> <input> <signature> <key-id-hex>
      exit 0 only if <signature> (SSHSIG) was made by the root key <key-id-hex> (release/runtime:
      the target key) of the canonical <root.json> over exactly <input> under that kind's
      namespace (production sshsig verifier)";

const MAX_KEY_FILE_BYTES: u64 = 4096;
const MAX_SIGNATURE_FILE_BYTES: u64 = 16 * 1024;

fn namespace(kind: &str) -> Option<&'static str> {
    match kind {
        "root" => Some(ROOT_SSHSIG_NAMESPACE),
        "release" => Some(RELEASE_SSHSIG_NAMESPACE),
        "runtime" => Some(RUNTIME_SSHSIG_NAMESPACE),
        "emergency" => Some(EMERGENCY_SSHSIG_NAMESPACE),
        "bundle" => Some(MIGRATION_BUNDLE_SSHSIG_NAMESPACE),
        _ => None,
    }
}

/// New file only (never an overwrite); `mode` applies on Unix.
fn create_new(path: &Path, mode: u32) -> Result<File, &'static str> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path).map_err(|_| "output exists or cannot be created")
}

fn write_new(path: &Path, mode: u32, content: &[u8]) -> Result<(), &'static str> {
    let mut file = create_new(path, mode)?;
    file.write_all(content).and_then(|()| file.sync_all()).map_err(|_| "cannot write output")
}

/// Regular file, at most `limit` bytes, read once.
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, &'static str> {
    let file = File::open(path).map_err(|_| "cannot open input")?;
    let metadata = file.metadata().map_err(|_| "cannot open input")?;
    if !metadata.is_file() || metadata.len() > limit { return Err("input is not a regular file within the size limit"); }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|_| "cannot read input")?;
    if bytes.len() as u64 != metadata.len() { return Err("input changed while it was read"); }
    Ok(bytes)
}

fn load_key(path: &Path) -> Result<TestKey, &'static str> {
    let bytes = read_bounded(path, MAX_KEY_FILE_BYTES)?;
    std::str::from_utf8(&bytes).ok().and_then(TestKey::parse).ok_or("not a duduclaw TEST-ONLY key file")
}

fn random_seed() -> Result<[u8; 32], &'static str> {
    let mut seed = [0u8; 32];
    File::open("/dev/urandom").and_then(|mut source| source.read_exact(&mut seed))
        .map_err(|_| "no system randomness")?;
    Ok(seed)
}

fn keygen(role: &str, path: &Path) -> Result<(), &'static str> {
    let role = Role::parse(role).ok_or("unknown role")?;
    let key = TestKey::from_seed(role, random_seed()?);
    let public = match (key.public(), key.minisign()) {
        (Some(public), _) => openssh_public_line(&public, &format!("duduclaw-TEST-ONLY-{}", role.name())),
        (None, Some(minisign)) => minisign.public_file(),
        (None, None) => return Err("unknown role"),
    };
    let mut public_path = path.as_os_str().to_owned();
    public_path.push(".pub");
    let public_path = Path::new(&public_path);
    if public_path.exists() { return Err("output exists or cannot be created"); }
    write_new(path, 0o600, key.to_file().as_bytes())?;
    write_new(public_path, 0o644, public.as_bytes())?;
    print!("{public}");
    Ok(())
}

fn sign(key: &Path, kind: &str, input: &Path, output: &Path) -> Result<(), &'static str> {
    let namespace = namespace(kind).ok_or("unknown document kind")?;
    let key = load_key(key)?;
    let message = read_bounded(input, MAX_METADATA_BYTES as u64)?;
    let signature = key.sign(namespace, &message).ok_or("this key role may not sign this document kind")?;
    write_new(output, 0o644, signature.as_bytes())
}

fn minisign(key: &Path, input: &Path, trusted_comment: &str, output: &Path) -> Result<(), &'static str> {
    use blake2::Digest;
    let signer = load_key(key)?.minisign().ok_or("not a minisign test key")?;
    let mut file = File::open(input).map_err(|_| "cannot open input")?;
    let metadata = file.metadata().map_err(|_| "cannot open input")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_MIGRATION_BUNDLE_BYTES {
        return Err("input is not a regular file within the size limit");
    }
    let mut hasher = blake2::Blake2b512::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| "cannot read input")?;
        if count == 0 { break; }
        hasher.update(buffer.get(..count).ok_or("cannot read input")?);
        total += count as u64;
    }
    if total != metadata.len() { return Err("input changed while it was read"); }
    let signature = signer.sign_prehash(hasher.finalize().as_slice(), trusted_comment)
        .ok_or("trusted comment must be one printable line")?;
    write_new(output, 0o644, signature.as_bytes())
}

/// Verification of one signature with the crate's production code (`sshsig::SshSignature`):
/// scripts/sign-isolation-manifest.py checks TEST signatures with this instead of skipping
/// them (`ssh-keygen -Y verify` is the production path). The signing key must be the root
/// key whose id the caller computed, and that id must be the one embedded in the signature.
fn verify(root: &Path, kind: &str, input: &Path, signature: &Path, key_id: &str) -> Result<(), &'static str> {
    let namespace = namespace(kind).ok_or("unknown document kind")?;
    let root_bytes = read_bounded(root, MAX_METADATA_BYTES as u64)?;
    let root: RootMetadata = decode_canonical(&root_bytes).map_err(|_| "root metadata is not canonical")?;
    let message = read_bounded(input, MAX_METADATA_BYTES as u64)?;
    let armor = String::from_utf8(read_bounded(signature, MAX_SIGNATURE_FILE_BYTES)?)
        .map_err(|_| "signature is not text")?;
    let signature = SshSignature::parse(&armor).map_err(|_| "malformed SSH signature")?;
    let expected: [u8; 32] = hex::decode(key_id).ok().and_then(|bytes| bytes.try_into().ok())
        .ok_or("key id must be 64 hex digits")?;
    if signature.key_id().0 != expected { return Err("the signature was made by another key"); }
    // Release and runtime manifests are signed by the target key of the root metadata
    // (scripts/sign-isolation-manifest.py checks the target signature before it writes);
    // every other kind only by a root key.
    let key = if matches!(kind, "release" | "runtime") {
        Some(&root.target_key).filter(|key| key.key_id.0 == expected).ok_or("not the target key of the root metadata")?
    } else {
        root.root_keys.iter().find(|key| key.key_id.0 == expected).ok_or("not a root key of the root metadata")?
    };
    signature.verify(key, namespace, &message).map_err(|_| "signature does not verify")?;
    Ok(())
}

fn run(args: &[String]) -> Result<(), &'static str> {
    match args {
        [command, root, kind, input, signature, key_id] if command == "verify" =>
            verify(Path::new(root), kind, Path::new(input), Path::new(signature), key_id),
        [command, role, path] if command == "keygen" => keygen(role, Path::new(path)),
        [command, key, kind, input, output] if command == "sign" =>
            sign(Path::new(key), kind, Path::new(input), Path::new(output)),
        [command, key, input, comment, output] if command == "minisign" =>
            minisign(Path::new(key), Path::new(input), comment, Path::new(output)),
        [command, key] if command == "minisign-public" => {
            let signer = load_key(Path::new(key))?.minisign().ok_or("not a minisign test key")?;
            println!("{}", signer.public());
            Ok(())
        }
        _ => Err(USAGE),
    }
}

fn main() -> ExitCode {
    let mut args = Vec::new();
    for argument in std::env::args_os().skip(1) {
        match argument.into_string() {
            Ok(argument) => args.push(argument),
            Err(_) => {
                eprintln!("test-signer: arguments must be UTF-8");
                return ExitCode::from(2);
            }
        }
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) if message == USAGE => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(message) => {
            eprintln!("test-signer: {message}");
            ExitCode::from(1)
        }
    }
}
