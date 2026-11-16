//! R1 trust-root initialisation (plan §4.1 `trust_init::publish`), called by R3
//! step S4 after the gateway is stopped and its processes are clear. Isolation
//! facts start here. Everything runs under STATE_ROOT/update.lock. Each step first
//! reads back what exists and skips it when already complete; anything present but
//! different refuses (no repair, no overwrite), so an interrupted run resumes by
//! calling `publish` again with the same receipt.
use std::{io::{Seek, SeekFrom}, os::unix::fs::PermissionsExt, path::Path};
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use crate::{Error, Result, publication::artifact_name, state::State, trust_store::TrustLedger};
use super::{bootstrap_verify::{self, Context, StageReceipt}, files::{Directory, copy_artifact, verify_fd},
    launcher::SlotReceipt, service::TrustBundle, CONFIG_ROOT, INSTALL_ROOT, STATE_ROOT};

/// Update-service subset of duduclaw-cli `protected_linux_layout::DIRECTORIES`,
/// parent first, with the same modes. (STATE_ROOT is created before the lock.)
pub const UPDATE_DIRECTORIES: &[(&str, u32)] = &[
    ("/etc/duduclaw-updated", 0o700),
    ("/var/lib/duduclaw-updated", 0o700),
    ("/opt/duduclaw-isolated", 0o755),
    ("/opt/duduclaw-isolated/generations", 0o755),
    ("/opt/duduclaw-isolated/slots", 0o700),
    ("/opt/duduclaw-isolated/slots/a", 0o700),
    ("/opt/duduclaw-isolated/slots/b", 0o700),
];
/// File name the launcher verifies in a slot directory.
const SLOT_BINARY: &str = "duduclaw-updated";
const SLOT_TEMPORARY: &str = "duduclaw-updated.next";
const STATE_FILE: &str = "state.json";
const LEDGER_FILE: &str = "trust-ledger.json";
/// Same bound as `Directory::json`.
const MAX_STATE_BYTES: usize = 1024 * 1024;

#[cfg(test)]
thread_local! { static STOP_AFTER: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) }; }
/// Test-only crash injection after a completed step.
fn checkpoint(step: u8) -> Result<()> {
    #[cfg(test)]
    if STOP_AFTER.with(|value| value.get() == Some(step)) {
        STOP_AFTER.with(|value| value.set(None));
        return Err(Error::Maintenance);
    }
    let _ = step;
    Ok(())
}

fn missing(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// Publish trust.json, slot A, state.json and `current` for the staged generation
/// and return its digest. The receipt must equal the re-verified staging area, and
/// its pin must be this build's compiled pin of the same kind (test vs production).
pub fn publish(stage: &StageReceipt) -> Result<Sha256Digest> {
    // First action: dumpable 0, NNP, umask 077, then real AND effective uid 0.
    // Nothing (receipt, stage, any path) is read before both hold.
    super::harden()?;
    super::require_root()?;
    let context = Context::production()?;
    publish_at(Path::new(bootstrap_verify::PROTECTED_ROOT), stage, &context)
}

pub(crate) fn publish_at(protected: &Path, stage: &StageReceipt, context: &Context) -> Result<Sha256Digest> {
    super::require_root()?;
    if stage.schema_version != 1 || stage.root_sha256 != context.pin || stage.verifier_pin_kind != context.pin_kind {
        return Err(Error::Signature);
    }
    let (receipt, staged) = bootstrap_verify::open_stage_at(protected, stage.manifest_digest, context)?;
    if receipt != *stage { return Err(Error::Signature); }
    let digest = stage.manifest_digest;
    let root = crate::trust::verify_root(&staged.root_bytes, &staged.root_signatures, context.pin, context.now)?;

    // 1. Update-service directories (STATE_ROOT first: it holds the lock).
    ensure_path(STATE_ROOT, 0o700)?;
    let state = Directory::open(Path::new(STATE_ROOT))?;
    let _lock = state.lock()?;
    for (path, mode) in UPDATE_DIRECTORIES { ensure_path(path, *mode)?; }
    checkpoint(1)?;

    // 2. Immutable generation from the staging area (verified, never selected here).
    let install = Directory::open(Path::new(INSTALL_ROOT))?;
    let published = super::bootstrap::publish_generation(&staged.artifacts, &install, &staged.manifest,
        &staged.manifest_bytes, &staged.manifest_signature)?;
    if published != digest { return Err(Error::Signature); }
    checkpoint(2)?;

    // 3. trust.json = BootstrapBundle{root, signatures, pin}.
    let config = Directory::open(Path::new(CONFIG_ROOT))?;
    write_trust(&config, &staged.root_bytes, &staged.root_signatures, context)?;
    checkpoint(3)?;

    // 4. Slot A executable and its receipt.
    install_slot_a(&install, &config, &root, &staged.manifest, &staged.manifest_bytes, &staged.manifest_signature,
        digest, context.now)?;
    checkpoint(4)?;

    // 5. state.json, read back byte-for-byte and through State::validate.
    write_state(&state, &install, &staged.manifest, &staged.manifest_bytes, &staged.manifest_signature, digest,
        context.now)?;
    checkpoint(5)?;

    // 6. current -> generations/<hex>, read back.
    super::activation::publish_initial_current(&install, digest)?;
    if super::activation::read_current()? != digest { return Err(Error::Maintenance); }
    checkpoint(6)?;

    // 7. trust-ledger.json, so later verification no longer depends on trust.json.
    record_ledger(&state, &staged.manifest_bytes, &staged.manifest_signature, context.now)?;
    checkpoint(7)?;
    Ok(digest)
}

fn ensure_path(path: &str, mode: u32) -> Result<Directory> {
    let path = Path::new(path);
    let parent = Directory::open(path.parent().ok_or(Error::Invalid)?)?;
    let name = path.file_name().and_then(|name| name.to_str()).ok_or(Error::Invalid)?;
    bootstrap_verify::ensure_dir(&parent, name, mode)
}

fn write_trust(config: &Directory, root_bytes: &[u8], signatures: &[String], context: &Context) -> Result<()> {
    let expected = TrustBundle { root: String::from_utf8(root_bytes.to_vec()).map_err(|_| Error::Invalid)?,
        signatures: signatures.to_vec(), pin: context.pin };
    let expected_bytes = serde_json::to_vec(&expected)?;
    match config.json::<TrustBundle>("trust.json") {
        Ok(existing) => if serde_json::to_vec(&existing)? != expected_bytes { return Err(Error::Maintenance); },
        Err(error) if missing(&error) => config.atomic_json("trust.json", &expected)?,
        Err(error) => return Err(error),
    }
    let written: TrustBundle = config.json("trust.json")?;
    if serde_json::to_vec(&written)? != expected_bytes { return Err(Error::Maintenance); }
    TrustLedger::bootstrap(written.root.as_bytes(), &written.signatures, written.pin, context.now)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_slot_a(install: &Directory, config: &Directory, root: &RootMetadata, manifest: &ReleaseManifest,
    manifest_bytes: &[u8], signature: &str, digest: Sha256Digest, now: u64) -> Result<()> {
    let artifact = manifest.artifacts.iter().find(|a| a.install_target == InstallTarget::UpdateSlot).ok_or(Error::Signature)?;
    if artifact_name(&artifact.install_target)? != SLOT_BINARY { return Err(Error::Invalid); }
    let slots = install.child("slots")?;
    let slot = slots.child("a")?;
    match slot.read(SLOT_BINARY) {
        Ok(_) => (),
        Err(error) if missing(&error) => {
            // Same .next + rename protocol as launcher::stage_inactive_slot.
            let generation = install.child("generations")?.child(&hex::encode(digest.0))?;
            let mut source = verify_fd(generation.read(SLOT_BINARY)?, artifact.size_bytes, artifact.sha256)?;
            source.seek(SeekFrom::Start(0))?;
            match slot.remove(SLOT_TEMPORARY) {
                Ok(()) => (),
                Err(error) if missing(&error) => (),
                Err(error) => return Err(error),
            }
            let mut target = slot.create(SLOT_TEMPORARY)?;
            copy_artifact(&mut source, &mut target, artifact)?;
            target.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            target.sync_all()?;
            drop(target);
            slot.rename(SLOT_TEMPORARY, &slot, SLOT_BINARY, false)?;
            slots.sync()?;
            install.sync()?;
        }
        Err(error) => return Err(error),
    }
    let installed = slot.read(SLOT_BINARY)?;
    if installed.metadata()?.permissions().mode() & 0o7777 != 0o700 { return Err(Error::Maintenance); }
    verify_fd(installed, artifact.size_bytes, artifact.sha256)?;
    let manifest_text = String::from_utf8(manifest_bytes.to_vec()).map_err(|_| Error::Invalid)?;
    match config.json::<SlotReceipt>("slot-a.json") {
        // An interrupted run keeps its original acceptance time.
        Ok(_) => (),
        Err(error) if missing(&error) => config.atomic_json("slot-a.json",
            &SlotReceipt { manifest: manifest_text.clone(), signature: signature.to_owned(), accepted_at_unix: now })?,
        Err(error) => return Err(error),
    }
    let receipt: SlotReceipt = config.json("slot-a.json")?;
    if receipt.manifest != manifest_text || receipt.signature != signature { return Err(Error::Maintenance); }
    crate::trust::verify_installed_release(root, manifest_bytes, signature, receipt.accepted_at_unix, now)?;
    Ok(())
}

pub(crate) fn initial_state(manifest: &ReleaseManifest, digest: Sha256Digest) -> State {
    State { schema_version: 1, product: manifest.product, arch: manifest.arch, floor: manifest.version,
        serial: manifest.serial, committed_digest: digest, highest_serial: manifest.serial, highest_digest: digest,
        pending: None, selector: SlotSelector { active: UpdateSlot::A, previous: UpdateSlot::A, trial_slot: None, attempts: 0 },
        maintenance: false, requests: vec![], resume_required: false, running_generation: None, running_version: None }
}

/// The canonical bytes of a state snapshot: exactly what `Directory::atomic_json` writes.
pub(crate) fn canonical_state(state: &State) -> Result<Vec<u8>> { Ok(serde_json::to_vec(state)?) }

/// Step 5. Until steps 6 and 7 have both proved this generation (`current` names
/// it AND the trust ledger records it), an existing state.json must be byte-for-byte
/// the initial state this function writes: a divergent snapshot left between step 5
/// and steps 6–7 (selector, pending, highest serial/digest, maintenance, requests,
/// ...) is refused, never adopted. Only afterwards, when the started service may
/// legitimately have moved runtime fields, is a different snapshot accepted, and
/// then only if `State::validate` passes and the committed identity is unchanged.
#[allow(clippy::too_many_arguments)]
fn write_state(state: &Directory, install: &Directory, manifest: &ReleaseManifest, manifest_bytes: &[u8],
    signature: &str, digest: Sha256Digest, now: u64) -> Result<()> {
    let expected = initial_state(manifest, digest);
    expected.validate()?;
    let expected_bytes = canonical_state(&expected)?;
    match state.bytes(STATE_FILE, MAX_STATE_BYTES) {
        Ok(existing) => {
            if existing == expected_bytes { return Ok(()); }
            let proven = later_steps_agree(state, install, manifest_bytes, signature, digest, now)?;
            return accept_existing_state(&existing, &expected, &expected_bytes, proven);
        }
        Err(error) if missing(&error) => state.atomic_json(STATE_FILE, &expected)?,
        Err(error) => return Err(error),
    }
    let written = state.bytes(STATE_FILE, MAX_STATE_BYTES)?;
    if written != expected_bytes { return Err(Error::Maintenance); }
    let parsed: State = serde_json::from_slice(&written)?;
    parsed.validate()
}

/// Pure decision for an existing state.json (see `write_state`).
pub(crate) fn accept_existing_state(existing: &[u8], expected: &State, expected_bytes: &[u8], later_steps_agree: bool)
    -> Result<()> {
    if existing == expected_bytes { return Ok(()); }
    if !later_steps_agree { return Err(Error::Maintenance); }
    let state: State = serde_json::from_slice(existing).map_err(|_| Error::Maintenance)?;
    state.validate()?;
    if state.schema_version != expected.schema_version || state.product != expected.product
        || state.arch != expected.arch || state.floor != expected.floor || state.serial != expected.serial
        || state.committed_digest != expected.committed_digest {
        return Err(Error::Maintenance);
    }
    Ok(())
}

/// True only when step 6 (`current` -> generations/<digest>) AND step 7 (a valid
/// trust ledger that records exactly this release) both hold. Absence of either is
/// `false`; any other read or validation failure refuses.
fn later_steps_agree(state: &Directory, install: &Directory, manifest_bytes: &[u8], signature: &str,
    digest: Sha256Digest, now: u64) -> Result<bool> {
    match super::activation::read_current_at(install) {
        Ok(current) if current == digest => (),
        Ok(_) => return Ok(false),
        Err(error) if missing(&error) => return Ok(false),
        Err(error) => return Err(error),
    }
    let ledger: TrustLedger = match state.json(LEDGER_FILE) {
        Ok(ledger) => ledger,
        Err(error) if missing(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    ledger.current(now)?;
    ledger.verify_installed(manifest_bytes, signature, now)?;
    Ok(true)
}

fn record_ledger(state: &Directory, manifest_bytes: &[u8], signature: &str, now: u64) -> Result<()> {
    let exists = match state.read(LEDGER_FILE) {
        Ok(_) => true,
        Err(error) if missing(&error) => false,
        Err(error) => return Err(error),
    };
    if !exists {
        // The ledger must be derivable from trust.json + the slot A receipt + state.
        super::trust_admin::ledger()?;
        super::trust_admin::record_release(manifest_bytes, signature)?;
    }
    let ledger: TrustLedger = state.json(LEDGER_FILE)?;
    ledger.current(now)?;
    ledger.verify_installed(manifest_bytes, signature, now)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::bootstrap_verify::{BootstrapMode, Step, verify_and_stage_with, tests::{host_fixture, root_config, Fixture}};

    #[test]
    fn existing_state_must_equal_initial_bytes_until_current_and_ledger_agree() {
        let manifest = crate::publication::tests::manifest();
        let digest = Sha256Digest([5; 32]);
        let expected = initial_state(&manifest, digest);
        expected.validate().unwrap();
        let bytes = canonical_state(&expected).unwrap();
        // The canonical form is exactly what Directory::atomic_json writes.
        assert_eq!(bytes, serde_json::to_vec(&expected).unwrap());
        let accept = |existing: &[u8], agreed: bool| accept_existing_state(existing, &expected, &bytes, agreed);
        for agreed in [false, true] { accept(&bytes, agreed).unwrap(); }

        // Runtime fields a started service may move. Each snapshot is itself valid,
        // yet none is adopted while current and the ledger have not both proved
        // this generation (a divergent state left between step 5 and steps 6-7).
        let mut divergent = Vec::new();
        let mut state = expected.clone();
        state.selector = SlotSelector { active: UpdateSlot::A, previous: UpdateSlot::A, trial_slot: Some(UpdateSlot::B), attempts: 1 };
        divergent.push(state);
        let mut state = expected.clone();
        state.selector = SlotSelector { active: UpdateSlot::B, previous: UpdateSlot::A, trial_slot: None, attempts: 0 };
        divergent.push(state);
        let mut state = expected.clone();
        let next = ReleaseVersion { patch: manifest.version.patch + 1, ..manifest.version };
        state.pending = Some(crate::state::Pending { version: next, serial: manifest.serial + 1, digest: Sha256Digest([6; 32]),
            phase: UpdateJournalState::Prepared, updater_slot: None, accepted_at_unix: 1, emergency: false });
        state.highest_serial = manifest.serial + 1;
        state.highest_digest = Sha256Digest([6; 32]);
        divergent.push(state);
        let mut state = expected.clone();
        state.highest_serial = manifest.serial + 3;
        state.highest_digest = Sha256Digest([7; 32]);
        divergent.push(state);
        let mut state = expected.clone();
        state.maintenance = true;
        divergent.push(state);
        let mut state = expected.clone();
        state.resume_required = true;
        divergent.push(state);
        for state in &divergent {
            state.validate().unwrap();
            let text = canonical_state(state).unwrap();
            assert_ne!(text, bytes);
            assert!(accept(&text, false).is_err());
            // After current and the ledger agree, runtime fields may differ.
            accept(&text, true).unwrap();
        }
        // Same values in another serialisation are not byte-equal: refused before agreement.
        let pretty = serde_json::to_vec_pretty(&expected).unwrap();
        assert!(accept(&pretty, false).is_err());
        accept(&pretty, true).unwrap();
        let mut trailing = bytes.clone();
        trailing.push(b'\n');
        assert!(accept(&trailing, false).is_err());

        // The committed identity never changes, even after agreement.
        let mut identity = Vec::new();
        let mut state = expected.clone();
        state.floor = ReleaseVersion { patch: manifest.version.patch + 1, ..manifest.version };
        identity.push(state);
        let mut state = expected.clone();
        state.serial += 1;
        state.highest_serial += 1;
        identity.push(state);
        let mut state = expected.clone();
        state.committed_digest = Sha256Digest([8; 32]);
        state.highest_digest = Sha256Digest([8; 32]);
        identity.push(state);
        let mut state = expected.clone();
        state.product = Product::DuduclawPro;
        identity.push(state);
        for state in &identity {
            let text = canonical_state(state).unwrap();
            assert!(accept(&text, false).is_err());
            assert!(accept(&text, true).is_err());
        }
        // Invalid or unreadable snapshots refuse.
        let mut invalid = expected.clone();
        invalid.schema_version = 2;
        assert!(accept(&canonical_state(&invalid).unwrap(), true).is_err());
        let mut invalid = expected.clone();
        invalid.highest_serial = manifest.serial;
        invalid.highest_digest = Sha256Digest([9; 32]);
        assert!(accept(&canonical_state(&invalid).unwrap(), true).is_err());
        for garbage in [&b""[..], b"{", b"{\"schema_version\":1}", b"null"] {
            assert!(accept(garbage, true).is_err());
        }
    }

    /// Production update-service paths are written: only on a disposable VM.
    fn disposable() {
        assert_eq!(nix::unistd::geteuid().as_raw(), 0, "requires root");
        assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
            "set DUDUCLAW_ISO_DISPOSABLE=1 only on a disposable VM");
    }
    fn updater_binary() -> Vec<u8> {
        let path = std::env::var("ISO_BOOTSTRAP_UPDATED_BINARY")
            .expect("set ISO_BOOTSTRAP_UPDATED_BINARY to a built duduclaw-updated executable");
        std::fs::read(path).unwrap()
    }
    /// Stage the deterministic test-key fixture (real duduclaw-updated as slot A).
    fn staged(temp: &Path) -> (Fixture, StageReceipt, Context, std::path::PathBuf) {
        let fixture = host_fixture(Some(updater_binary()));
        let empty = temp.join("facts-root");
        std::fs::create_dir(&empty).unwrap();
        let config = root_config(temp, &fixture, empty);
        let mut step = Step::Harden;
        let staged = verify_and_stage_with(&config, BootstrapMode::Install, &mut step).unwrap();
        let protected = config.paths.protected.clone();
        (fixture, staged.receipt, config.context, protected)
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM (DUDUCLAW_ISO_DISPOSABLE=1, ISO_BOOTSTRAP_UPDATED_BINARY); publishes the production update-service facts"]
    fn iso_root_bootstrap_trust_init_resumable_each_step() {
        disposable();
        let temp = tempfile::Builder::new().prefix("duduclaw-trust-init-").tempdir_in("/root").unwrap();
        let (fixture, receipt, context, protected) = staged(temp.path());
        // Interrupt after every step in turn; each rerun skips what is complete.
        for stop in 1..=7u8 {
            STOP_AFTER.with(|value| value.set(Some(stop)));
            assert!(publish_at(&protected, &receipt, &context).is_err(), "stop after step {stop}");
            STOP_AFTER.with(|value| value.set(None));
        }
        let digest = publish_at(&protected, &receipt, &context).unwrap();
        assert_eq!(digest, receipt.manifest_digest);
        assert_eq!(publish_at(&protected, &receipt, &context).unwrap(), digest);
        assert_eq!(crate::linux::activation::read_current().unwrap(), digest);
        let state: State = Directory::open(Path::new(STATE_ROOT)).unwrap().json("state.json").unwrap();
        state.validate().unwrap();
        assert_eq!((state.committed_digest, state.floor, state.serial), (digest, fixture.manifest.version, fixture.manifest.serial));
        assert_eq!(state.selector.active, UpdateSlot::A);
        crate::linux::trust_admin::load_current().unwrap();
        crate::linux::trust_admin::verify_recorded(&fixture.manifest_bytes, &fixture.manifest_signature).unwrap();
        // Any receipt other than the re-verified stage is refused before a write.
        for forged in [StageReceipt { bundle_size: receipt.bundle_size + 1, ..receipt.clone() },
            StageReceipt { verifier_pin_kind: bootstrap_verify::PinKind::Production, ..receipt.clone() },
            StageReceipt { root_sha256: Sha256Digest([0; 32]), ..receipt.clone() }] {
            assert!(publish_at(&protected, &forged, &context).is_err());
        }
    }

    #[test]
    #[ignore = "requires root native Linux on a disposable VM with accounts duduclaw and duduclaw-dl (DUDUCLAW_ISO_DISPOSABLE=1, ISO_BOOTSTRAP_UPDATED_BINARY)"]
    fn iso_root_bootstrap_launcher_boots_slot_a() {
        use std::{process::Stdio, time::{Duration, Instant}};
        disposable();
        let temp = tempfile::Builder::new().prefix("duduclaw-launcher-").tempdir_in("/root").unwrap();
        let (_, receipt, context, protected) = staged(temp.path());
        let digest = publish_at(&protected, &receipt, &context).unwrap();
        let state_dir = Directory::open(Path::new(STATE_ROOT)).unwrap();
        match state_dir.remove("self-test.json") {
            Ok(()) => (),
            Err(error) if missing(&error) => (),
            Err(error) => panic!("cannot reset self-test marker: {error}"),
        }
        let binary = std::env::var("ISO_BOOTSTRAP_UPDATED_BINARY").unwrap();
        let mut child = std::process::Command::new(binary).arg("--launch").env_clear()
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        // The slot binary writes self-test.json only after verify_executing_slot
        // proved it is running the verified slot A inode via --serve-slot a.
        let deadline = Instant::now() + Duration::from_secs(30);
        let booted = loop {
            if state_dir.read("self-test.json").is_ok() { break true; }
            if child.try_wait().unwrap().is_some() || Instant::now() >= deadline { break false; }
            std::thread::sleep(Duration::from_millis(100));
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(booted, "launcher did not boot slot A from the initialised state");
        let state: State = state_dir.json("state.json").unwrap();
        state.validate().unwrap();
        assert_eq!((state.selector.active, state.selector.trial_slot, state.maintenance), (UpdateSlot::A, None, false));
        assert_eq!(state.committed_digest, digest);
    }
}
