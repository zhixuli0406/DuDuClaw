//! Single-writer daemon; a bounded request history survives process restarts.
use std::{path::Path, sync::{Arc, Mutex}, thread};
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, update::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{Error, Result, state::{self, State, GenerationActivator}};
use super::{files::Directory, download::Downloader, transport, STATE_ROOT, CONFIG_ROOT, INSTALL_ROOT};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBundle { pub root: String, pub signatures: Vec<String>, pub pin: Sha256Digest }
pub fn load_trust() -> Result<RootMetadata> {
    super::trust_admin::load_current()
}
pub fn accounts() -> Result<(u32, u32, Downloader)> {
    use nix::unistd::User;
    let gateway = User::from_name(duduclaw_isolation::ids::GATEWAY_USER).map_err(transport::errno)?.ok_or(Error::Maintenance)?;
    let downloader = User::from_name(duduclaw_isolation::ids::DOWNLOAD_USER).map_err(transport::errno)?.ok_or(Error::Maintenance)?;
    if gateway.uid.as_raw() == 0 || gateway.gid.as_raw() == 0 || gateway.uid == downloader.uid { return Err(Error::Maintenance); }
    Ok((gateway.uid.as_raw(), gateway.gid.as_raw(), Downloader::new(downloader.uid.as_raw(), downloader.gid.as_raw())?))
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Incoming { Update(UpdateRequest), Probe(crate::readiness::ProbeRequest) }
struct Service { state: State, disk: state::FailClosedDisk<Directory>, working: bool }
impl Service {
    fn mutate<T>(&mut self, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let result = state::transaction(&mut self.state, &mut self.disk, f);
        // After uncertain fsync/rename failure, fail closed for the remainder of
        // this process; the next boot reads the authoritative on-disk snapshot.
        if self.disk.poisoned() { self.state.maintenance = true; }
        result
    }
    fn status(&mut self, id: RequestId, status: UpdateStatus) -> Result<()> {
        self.mutate(|state| state.set_status(id, status))
    }
}
pub fn run(slot: UpdateSlot) -> Result<()> {
    super::harden()?;
    let mut disk = Directory::open(Path::new(STATE_ROOT))?;
    let _lock = disk.lock()?;
    let mut state: State = disk.json("state.json")?;
    state.validate()?;
    let host_arch = if cfg!(target_arch = "x86_64") { Architecture::X86_64 }
        else if cfg!(target_arch = "aarch64") { Architecture::Aarch64 } else { return Err(Error::Incompatible); };
    if state.arch != host_arch { return Err(Error::Incompatible); }
    let root = load_trust()?;
    super::launcher::verify_executing_slot(&state, slot, &root)?;
    // The real first root run checks trust, signed known manifest, writable
    // journal and dir fsync before marking health; no version floor moves here.
    disk.atomic_json("self-test.json", &serde_json::json!({"event":"updater_self_test"}))?;
    let (gateway_uid, gateway_gid, _) = accounts()?;
    let listener = transport::bind(gateway_gid)?;
    if state.selector.trial_slot == Some(slot) { state.mark_healthy(slot)?; }
    state.interrupted_requests();
    use state::DurableState;
    disk.save(&state)?;
    let mut disk = state::FailClosedDisk::new(disk);
    super::emergency::reconcile(&mut state, &mut disk)?;
    let recovery = state.recovery_request()?;
    let shared = Arc::new(Mutex::new(Service { state, disk, working: recovery.is_some() }));
    // The launcher already verified the executing updater. Reconcile the product
    // selector before accepting work, even when the journal has no pending row.
    recover_committed(&shared)?;
    if let Some((id, version)) = recovery { start_worker(Arc::clone(&shared), id, version); }
    loop {
        let fd = match transport::accept(&listener, gateway_uid, gateway_gid) { Ok(fd) => fd, Err(_) => continue };
        let request: Incoming = match transport::receive(&fd) { Ok(r) => r, Err(_) => continue };
        let request = match request {
            Incoming::Update(UpdateRequest::V1(request)) => request,
            Incoming::Probe(request) => {
                let evidence = (|| -> Result<crate::readiness::GenerationEvidence> {
                    if request.readiness_schema != crate::readiness::READINESS_SCHEMA || request.nonce.0 == [0; 16] {
                        return Err(Error::Invalid);
                    }
                    let service = shared.lock().map_err(|_| Error::Maintenance)?;
                    if service.working || service.disk.poisoned() { return Err(Error::Maintenance); }
                    let root = load_trust()?;
                    super::launcher::verify_executing_slot(&service.state, slot, &root)?;
                    let (digest, manifest) = verified_state_generation(&service.state, false)?;
                    Ok(crate::readiness::GenerationEvidence { generation: digest, product: manifest.product,
                        version: manifest.version, protocols: manifest.protocols })
                })().ok();
                let _ = transport::send(&fd, &crate::readiness::ProbeResponse {
                    readiness_schema: crate::readiness::READINESS_SCHEMA, nonce: request.nonce, evidence });
                continue;
            }
        };
        let mut service = shared.lock().map_err(|_| Error::Maintenance)?;
        let mut start = None;
        let response = match request {
            UpdateRequestV1::Status { request_id } if service.disk.poisoned() =>
                UpdateResponseV1::Error { request_id, code: UpdateErrorCode::MaintenanceRequired },
            UpdateRequestV1::Status { request_id } => match service.state.status(request_id) {
                Some(state) => UpdateResponseV1::Status { request_id, state },
                None => UpdateResponseV1::Error { request_id, code: UpdateErrorCode::UnknownRequest },
            },
            UpdateRequestV1::Apply { version, request_id } => {
                if service.state.maintenance {
                    UpdateResponseV1::Error { request_id, code: UpdateErrorCode::MaintenanceRequired }
                } else if let Some(record) = service.state.requests.iter().find(|r| r.id == request_id) {
                    if record.version != version {
                        UpdateResponseV1::Error { request_id, code: UpdateErrorCode::InvalidRequest }
                    } else { UpdateResponseV1::Status { request_id, state: service.state.status(request_id).unwrap_or(UpdateStatus::Maintenance) } }
                } else if service.working {
                    UpdateResponseV1::Error { request_id, code: UpdateErrorCode::Busy }
                } else {
                    // Policy refusal does not poison the service; persistence
                    // failures do. Admission is checked on a separate snapshot.
                    let mut proposed = service.state.clone();
                    match proposed.admit(request_id, version) {
                        Err(e) => UpdateResponseV1::Error { request_id, code: e.code() },
                        Ok(_) => match service.disk.save(&proposed) {
                            Err(_) => { service.state.maintenance = true;
                                UpdateResponseV1::Error { request_id, code: UpdateErrorCode::MaintenanceRequired } }
                            Ok(()) => {
                                service.state = proposed;
                                service.working = true;
                                start = Some((request_id, version));
                                UpdateResponseV1::Accepted { request_id }
                            }
                        },
                    }
                }
            }
        };
        drop(service);
        // Acceptance is durable even if the requester disconnects before reply.
        let _ = transport::send(&fd, &UpdateResponse::V1(response));
        if let Some((id, version)) = start {
            start_worker(Arc::clone(&shared), id, version);
        }
    }
}
enum InstallOutcome { Complete, RestartUpdater }
fn start_worker(shared: Arc<Mutex<Service>>, id: RequestId, version: ReleaseVersion) {
    thread::spawn(move || {
        let committed = match shared.lock() { Ok(s) => s.state.effective_digest(), Err(_) => return };
        let mut activator = super::activation::LinuxGenerationActivator::new(committed);
        let outcome = install(&shared, id, version, &mut activator);
        if matches!(outcome, Ok(InstallOutcome::RestartUpdater)) {
            // Restart=on-failure re-enters the frozen launcher and consumes the
            // durable one-shot trial. No generation has been switched yet.
            std::process::exit(75);
        }
        if let Ok(mut service) = shared.lock() {
            if outcome.is_err() {
                let status = if service.state.pending.is_some() || service.state.resume_required || service.disk.poisoned()
                    || matches!(outcome, Err(Error::Maintenance)) { UpdateStatus::Maintenance } else { UpdateStatus::Failed };
                let _ = service.status(id, status);
            }
            service.working = false;
        }
    });
}
fn installed_generation(digest: Sha256Digest) -> Result<(Directory, Vec<u8>, String)> {
    let generation = Directory::open(Path::new(INSTALL_ROOT))?.child("generations")?.child(&hex::encode(digest.0))?;
    let bytes = generation.bytes("manifest.json", 256 * 1024)?;
    if Sha256Digest(Sha256::digest(&bytes).into()) != digest { return Err(Error::Signature); }
    let signature = String::from_utf8(generation.bytes("manifest.json.sig", 16 * 1024)?).map_err(|_| Error::Invalid)?;
    Ok((generation, bytes, signature))
}
fn verify_artifacts(generation: &Directory, manifest: &ReleaseManifest) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    for artifact in &manifest.artifacts {
        let file = generation.read(&artifact_name(&artifact.install_target)?)?;
        if file.metadata()?.mode() & 0o7777 != u32::from(artifact.mode) { return Err(Error::Signature); }
        super::files::verify_fd(file, artifact.size_bytes, artifact.sha256)?;
    }
    generation.sync()
}
/// Read-only root publication: verify trust, immutable installed bytes, and the
/// committed selector. This proves F's generation, not NSS/SSH or mount policy.
pub fn verified_committed_generation() -> Result<(Sha256Digest, ReleaseManifest)> {
    let disk = Directory::open(Path::new(STATE_ROOT))?;
    let before: State = disk.json("state.json")?;
    let result = verified_state_generation(&before, true)?;
    let after: State = disk.json("state.json")?;
    after.validate()?;
    if after.effective_digest() != result.0 || after.pending.is_some() || after.maintenance || after.resume_required {
        return Err(Error::Maintenance);
    }
    Ok(result)
}
fn verified_state_generation(state: &State, verify_all_artifacts: bool) -> Result<(Sha256Digest, ReleaseManifest)> {
    state.validate()?;
    if state.maintenance || state.pending.is_some() || state.resume_required { return Err(Error::Maintenance); }
    let digest = state.effective_digest();
    if super::activation::read_current()? != digest { return Err(Error::Maintenance); }
    let (generation, bytes, signature) = installed_generation(digest)?;
    let manifest = super::trust_admin::verify_recorded(&bytes, &signature)?;
    if manifest.product != state.product || manifest.arch != state.arch
        || manifest.version != state.running_version.unwrap_or(state.floor) { return Err(Error::Incompatible); }
    // Clean service boot already verified every artifact. A health request
    // rechecks signed identity and the executing updater rather than hashing up
    // to 8 GiB under the service lock on every gateway readiness tick.
    if verify_all_artifacts { verify_artifacts(&generation, &manifest)?; }
    if super::activation::read_current()? != digest { return Err(Error::Maintenance); }
    Ok((digest, manifest))
}
fn recover_committed(shared: &Arc<Mutex<Service>>) -> Result<()> {
    let (committed, pending, resume) = {
        let service = shared.lock().map_err(|_| Error::Maintenance)?;
        (service.state.effective_digest(), service.state.pending.as_ref().map(|p| p.digest), service.state.resume_required)
    };
    let mut activator = super::activation::LinuxGenerationActivator::new(committed);
    if pending.is_some() {
        match super::activation::read_current() {
            Ok(current) if current == committed || Some(current) == pending => return Ok(()),
            _ => { let _ = activator.close_admission(committed); return Err(Error::Maintenance); }
        }
    }
    // Every clean boot checks the actual running services. A matching symlink
    // alone cannot establish a consistent generation. Leave admission closed if
    // any bytes, trust, identity or live readback cannot be checked.
    activator.close_admission(committed)?;
    let (generation, bytes, signature) = installed_generation(committed)?;
    let manifest = super::trust_admin::verify_recorded(&bytes, &signature)?;
    verify_artifacts(&generation, &manifest)?;
    {
        let service = shared.lock().map_err(|_| Error::Maintenance)?;
        super::emergency::complete_committed(&service.state)?;
    }
    activator.verify_committed(&manifest, committed)?;
    if resume {
        shared.lock().map_err(|_| Error::Maintenance)?.mutate(|state| state.finish_resume())?;
    }
    Ok(())
}
fn install(shared: &Arc<Mutex<Service>>, id: RequestId, version: ReleaseVersion,
    activator: &mut impl GenerationActivator) -> Result<InstallOutcome> {
    let pending = shared.lock().map_err(|_| Error::Maintenance)?.state.pending.clone();
    let (generation, bytes, signature, manifest, digest) = if let Some(pending) = pending {
        if pending.version != version { return Err(Error::Maintenance); }
        let (generation, bytes, signature) = installed_generation(pending.digest)?;
        let manifest = super::trust_admin::verify_installed(&bytes, &signature, pending.accepted_at_unix)?;
        let mut service = shared.lock().map_err(|_| Error::Maintenance)?;
        service.state.check_pending_manifest(&manifest, pending.digest)?;
        super::emergency::validate_pending(&service.state)?;
        service.mutate(|state| state.resume_request(id))?;
        (generation, bytes, signature, manifest, pending.digest)
    } else {
        download_generation(shared, id, version, activator)?
    };
    verify_artifacts(&generation, &manifest)?;
    // An accepted generation may finish after its metadata expires; current root
    // expiry, freeze/revocations and historical signing identity remain enforced.
    {
        let service = shared.lock().map_err(|_| Error::Maintenance)?;
        super::emergency::validate_pending(&service.state)?;
        if load_trust()?.frozen && !service.state.pending.as_ref().is_some_and(|p| p.emergency) { return Err(Error::Maintenance); }
    }
    let needs_slot = shared.lock().map_err(|_| Error::Maintenance)?.state.pending.as_ref()
        .is_some_and(|p| p.phase == UpdateJournalState::Prepared);
    if needs_slot {
        let mut service = shared.lock().map_err(|_| Error::Maintenance)?;
        let Service { state, disk, .. } = &mut *service;
        super::launcher::stage_inactive_slot(state, disk, &bytes, &signature)?;
        if state.selector.trial_slot.is_some() { return Ok(InstallOutcome::RestartUpdater); }
    }
    // Repeat the whole idempotent activation/readback after a crash at current,
    // any unit restart, Activated save, or commit rename. Never trust old health.
    super::emergency::validate_pending(&shared.lock().map_err(|_| Error::Maintenance)?.state)?;
    activator.activate_and_read_back(&manifest, digest)?;
    let mut service = shared.lock().map_err(|_| Error::Maintenance)?;
    if service.state.pending.as_ref().is_some_and(|p| p.phase == UpdateJournalState::Prepared) {
        service.mutate(|state| { state.activated(digest)?; state.set_status(id, UpdateStatus::Activated) })?;
    }
    generation.sync()?;
    Directory::open(Path::new(INSTALL_ROOT))?.child("generations")?.sync()?;
    service.mutate(|state| { state.commit(digest)?; state.set_status(id, UpdateStatus::Committed) })?;
    super::emergency::complete_committed(&service.state)?;
    drop(service);
    activator.finish_after_commit(&manifest, digest)?;
    shared.lock().map_err(|_| Error::Maintenance)?.mutate(|state| state.finish_resume())?;
    Ok(InstallOutcome::Complete)
}
fn download_generation(shared: &Arc<Mutex<Service>>, id: RequestId, version: ReleaseVersion,
    activator: &mut impl GenerationActivator) -> Result<(Directory, Vec<u8>, String, ReleaseManifest, Sha256Digest)> {
    let (product, arch) = {
        let mut service = shared.lock().map_err(|_| Error::Maintenance)?;
        if service.state.status(id) == Some(UpdateStatus::Queued) { service.status(id, UpdateStatus::Downloading)?; }
        (service.state.product, service.state.arch)
    };
    let root = load_trust()?;
    let (_, _, downloader) = accounts()?;
    let bytes = downloader.release(product, arch, version, "manifest.json")?.bytes(256 * 1024)?;
    let signature_bytes = downloader.release(product, arch, version, "manifest.json.signatures.json")?.bytes(16 * 1024)?;
    let signatures: Vec<String> = serde_json::from_slice(&signature_bytes)?;
    if signatures.len() != 1 { return Err(Error::Signature); }
    let signature = signatures[0].clone();
    let manifest = crate::trust::verify_release(&root, &bytes, &signature, super::now()?)?;
    if manifest.version != version { return Err(Error::Signature); }
    let digest = Sha256Digest(Sha256::digest(&bytes).into());
    shared.lock().map_err(|_| Error::Maintenance)?.state.check_manifest(&manifest, digest, super::now()?)?;
    {
        let service = shared.lock().map_err(|_| Error::Maintenance)?;
        if service.state.running_generation.is_some() {
            let (_, current_bytes, current_signature) = installed_generation(service.state.effective_digest())?;
            let running = super::trust_admin::verify_recorded(&current_bytes, &current_signature)?;
            if manifest.minimum_version > running.version { return Err(Error::Incompatible); }
        }
    }
    activator.preflight(&manifest, digest)?;
    let root_dir = Directory::open(Path::new(INSTALL_ROOT))?;
    let generations = root_dir.child("generations")?;
    let generation_name = hex::encode(digest.0);
    cleanup_generations(shared, &root_dir, &generations, digest)?;
    let existing = match generations.child(&generation_name) {
        Ok(dir) => Some(dir),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let generation = if let Some(generation) = existing { generation } else {
        let total = manifest.artifacts.iter().try_fold(0u64, |total, a| total.checked_add(a.size_bytes)).ok_or(Error::Invalid)?;
        root_dir.reserve_space(total.checked_add(crate::state::MAX_ARTIFACT_BYTES).ok_or(Error::Invalid)?)?;
        let stage = root_dir.create_child("pending-generation")?;
        for artifact in &manifest.artifacts {
            let mut output = stage.create(&artifact_name(&artifact.install_target)?)?;
            downloader.release(product, arch, version, &artifact.id)?.artifact(&mut output, artifact)?;
        }
        use std::io::Write;
        let mut metadata = stage.create("manifest.json")?;
        metadata.write_all(&bytes)?; metadata.sync_all()?;
        let mut sig = stage.create("manifest.json.sig")?;
        sig.write_all(signature.as_bytes())?; sig.sync_all()?;
        stage.sync()?; root_dir.sync()?;
        root_dir.rename("pending-generation", &generations, &generation_name, false)?;
        generations.child(&generation_name)?
    };
    if generation.bytes("manifest.json", 256 * 1024)? != bytes
        || generation.bytes("manifest.json.sig", 16 * 1024)? != signature.as_bytes() { return Err(Error::Signature); }
    verify_artifacts(&generation, &manifest)?;
    generations.sync()?;
    super::trust_admin::record_release(&bytes, &signature)?;
    shared.lock().map_err(|_| Error::Maintenance)?.mutate(|state| {
        state.prepare(&manifest, digest, super::now()?)?;
        state.set_status(id, UpdateStatus::Prepared)
    })?;
    Ok((generation, bytes, signature, manifest, digest))
}
fn cleanup_generations(shared: &Arc<Mutex<Service>>, root: &Directory, generations: &Directory,
    candidate: Sha256Digest) -> Result<()> {
    // Single writer, bounded flat stage. A crash during unlink is resumed next
    // time; no release or selector can reference pending-generation.
    match root.remove_artifact_directory("pending-generation") {
        Ok(()) => (), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(e),
    }
    let service = shared.lock().map_err(|_| Error::Maintenance)?;
    let mut keep = vec![hex::encode(service.state.committed_digest.0), hex::encode(service.state.effective_digest().0), hex::encode(candidate.0)];
    if let Some(pending) = &service.state.pending { keep.push(hex::encode(pending.digest.0)); }
    // Retain both slot generations so launcher rollback and emergency recovery
    // cannot lose their immutable source. Historical signatures stay in trust.
    let config = Directory::open(Path::new(CONFIG_ROOT))?;
    for name in ["slot-a.json", "slot-b.json"] {
        match config.json::<super::launcher::SlotReceipt>(name) {
            Ok(receipt) => keep.push(hex::encode(Sha256::digest(receipt.manifest.as_bytes()))),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    let current = hex::encode(super::activation::read_current()?.0);
    if !keep.iter().any(|entry| entry == &current) { return Err(Error::Maintenance); }
    for entry in generations.entries(64)? {
        if entry.len() != 64 || !entry.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return Err(Error::Maintenance); }
        if !keep.iter().any(|name| name == &entry) { generations.remove_artifact_directory(&entry)?; }
    }
    Ok(())
}
pub fn artifact_name(target: &InstallTarget) -> Result<String> {
    Ok(match target {
        InstallTarget::Gateway => "duduclaw".into(),
        InstallTarget::Spawnd => "duduclaw-spawnd".into(),
        InstallTarget::Trampoline => "duduclaw-run".into(),
        InstallTarget::Egress => "duduclaw-egress".into(),
        InstallTarget::EgressForwarder => "duduclaw-egress-forwarder".into(),
        InstallTarget::McpBridge => "duduclaw-mcp-bridge".into(),
        InstallTarget::ParseWorker => "duduclaw-work".into(),
        InstallTarget::Importer => "duduclaw-import".into(),
        InstallTarget::Migration => "duduclaw-migrate".into(),
        InstallTarget::UpdateSlot => "duduclaw-updated".into(),
        InstallTarget::RuntimeEntry { registry_id } => {
            if registry_id.is_empty() || registry_id.len() > 128 || !registry_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) { return Err(Error::Invalid); }
            format!("runtime-{registry_id}")
        },
    })
}
/// Operator-only nightly discovery. The unsigned index only selects a candidate;
/// Apply still verifies an official manifest and monotonic root-owned floor.
pub fn check_latest(dry_run: bool) -> Result<()> {
    super::harden()?;
    let (_, _, downloader) = accounts()?;
    let value: serde_json::Value = serde_json::from_slice(&downloader.latest()?.bytes(1024 * 1024)?)?;
    if value.get("prerelease").and_then(|v| v.as_bool()) != Some(false)
        || value.get("draft").and_then(|v| v.as_bool()) != Some(false) { return Err(Error::Invalid); }
    let version = crate::parse_version(value.get("tag_name").and_then(|v| v.as_str()).ok_or(Error::Invalid)?)?;
    let state: State = Directory::open(Path::new(STATE_ROOT))?.json("state.json")?;
    state.validate()?;
    if state.maintenance { return Err(Error::Maintenance); }
    if version <= state.floor { println!("already_current"); return Ok(()); }
    if dry_run { println!("candidate {}", crate::version_text(version)); return Ok(()); }
    let mut bytes = [0; 16];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let response = crate::client::request(UpdateRequest::V1(UpdateRequestV1::Apply { version, request_id: RequestId(bytes) }))?;
    match response {
        UpdateResponse::V1(UpdateResponseV1::Accepted { .. }) => { println!("queued"); Ok(()) }
        _ => Err(Error::Maintenance),
    }
}
