//! Root-only crash journal and atomic, expiring gateway readiness evidence.
use std::{fs::{self, File, OpenOptions}, io::{self, Read, Write}, os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt}}, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use duduclaw_isolation::{ids::{EmployeeId, RunId}, ipc::ErrorCode};
use crate::{registry::{root_path, REGISTRY_DIRECTORY}, readiness::Readiness};
pub const READINESS_PATH: &str = "/run/duduclaw-spawnd/readiness.json";
const MAX_BYTES: u64 = 64 * 1024;
fn invalid() -> io::Error { io::Error::other("invalid spawnd state") }

pub fn atomic_json<T: Serialize>(path: &Path, value: &T, gid: u32, mode: u32) -> io::Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    root_path(parent, true)?;
    let temp = path.with_extension("next");
    match fs::symlink_metadata(&temp) {
        Ok(_) => { root_path(&temp, false)?; fs::remove_file(&temp)?; },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}, Err(e) => return Err(e),
    }
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    if bytes.len() as u64 > MAX_BYTES { return Err(invalid()); }
    let mut file = OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW).open(&temp)?;
    if unsafe { libc::fchown(file.as_raw_fd(), 0, gid) } != 0 || unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    file.write_all(&bytes)?; file.sync_all()?;
    fs::rename(&temp, path)?; File::open(parent)?.sync_all()
}
pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, gid: u32, mode: u32) -> io::Result<T> {
    root_path(path, false)?;
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    let meta = file.metadata()?;
    if meta.uid() != 0 || meta.gid() != gid || meta.mode() & 0o7777 != mode || meta.nlink() != 1 || !meta.is_file() { return Err(invalid()); }
    let mut bytes = Vec::new(); file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES { return Err(invalid()); }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord { pub run: RunId, pub employee: EmployeeId, pub uid: u32 }
pub struct RunJournal { path: PathBuf, pub records: Vec<RunRecord>, poisoned: bool }
impl RunJournal {
    pub fn open() -> io::Result<Self> { Self::open_at(Path::new(REGISTRY_DIRECTORY)) }
    fn open_at(directory: &Path) -> io::Result<Self> {
        let path = directory.join("runs.json");
        let records = match read_json(&path, 0, 0o600) {
            Ok(records) => records, Err(e) if e.kind() == io::ErrorKind::NotFound => vec![], Err(e) => return Err(e),
        };
        let journal = Self { path, records, poisoned: false }; journal.validate()?; Ok(journal)
    }
    fn validate(&self) -> io::Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        if self.records.len() > 128 || self.records.iter().any(|r| !ids.insert(r.run) || r.uid == 0) { return Err(invalid()); }
        Ok(())
    }
    pub fn add(&mut self, record: RunRecord) -> io::Result<()> {
        if self.records.iter().any(|r| r.run == record.run || r.employee == record.employee) { return Err(invalid()); }
        self.records.push(record); self.save()
    }
    pub fn remove(&mut self, run: RunId) -> io::Result<()> { self.records.retain(|r| r.run != run); self.save() }
    fn save(&mut self) -> io::Result<()> {
        if self.poisoned { return Err(invalid()); }
        self.validate()?;
        let result = atomic_json(&self.path, &self.records, 0, 0o600);
        if result.is_err() { self.poisoned = true; }
        result
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandlockEvidence { pub detected_abi: u32, pub selected_abi: u32 }
impl LandlockEvidence {
    fn valid(&self) -> bool { self.detected_abi > 0 && self.selected_abi == self.detected_abi.min(7) }
}
/// The one protocol feature set (`ipc::SpawndFeatures`, a u32 bitset:
/// `helper_calls` = bit 0, `work_broker` = bit 1). The readiness report and
/// every `Reserved` carry the same value from `Host::features`; an older
/// reader ignores newer bits, and a missing set reads as none (the gateway
/// then reports the named readiness reason instead of sending).
pub use duduclaw_isolation::ipc::SpawndFeatures;
/// What this binary always decodes; `Core::refresh` adds `helper_calls` when
/// helpers can run on this host (launcher plans active, slot cgroups usable).
pub const DECODED_FEATURES: SpawndFeatures = SpawndFeatures::WORK_BROKER;
/// HX6 gate (D8 revision §4), security fix round finding 4: compile-time kill
/// switch, ANDed with this host's recording probe ([`RecordingProbeEvidence`]).
/// While `false` the probe never runs, `helper_recording` is never advertised
/// and spawnd refuses every recording-slot `HelperCall`
/// (`helper_recording_unsupported`). Flip only after the H15 acceptance of
/// `iso_root_helper_chromium_*` on Ubuntu 22.04/24.04 and Debian 12; even then
/// a host advertises recording only once its own probe passed.
pub const RECORDING_BUILD_ENABLED: bool = false;
/// Finding 4: the per-host recording probe (`host::probe`): spawnd ran the
/// signed recording helper in the rec slot of a real run, through that run's
/// launcher (F_L ∩ F_H, launcher + helper Landlock, slot cgroup) and the
/// helper's nested browser sandbox, against a fixed local page; `passed` only
/// if Chromium started, rendered and exited cleanly (exit 0, exact reply,
/// empty stderr). Bound to the boot and the Landlock ABI it ran under: a
/// report whose boot or ABI differs does not carry the bit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingProbeEvidence { pub boot_id: String, pub landlock_abi: u32, pub passed: bool }
/// HX6 security round 3 (N4): the published report's
/// [`EvidenceReport::helper_recording`] value, stored by `Core::refresh` right
/// after it publishes the report and read by `LinuxHost::recording_ready`
/// without the view lock, so lock contention can no longer make a ready host
/// answer `helper_recording_unsupported`. Starts closed; closed on a poisoned
/// core (and on drop of the core), i.e. only on a real "not ready".
#[derive(Clone, Default)]
pub struct RecordingGate(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl RecordingGate {
    pub fn publish(&self, report: &EvidenceReport, poisoned: bool) {
        self.0.store(!poisoned && report.helper_recording(), std::sync::atomic::Ordering::Release);
    }
    pub fn ready(&self) -> bool { self.0.load(std::sync::atomic::Ordering::Acquire) }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReport {
    pub version: u16,
    pub boot_id: String,
    pub checked_boottime_ms: u64,
    pub valid_for_ms: u64,
    pub host_pty_budget: u32,
    #[serde(default)]
    pub landlock: Option<LandlockEvidence>,
    /// [F17, H4] feature bits; absent in reports of an older spawnd ⇒ none.
    #[serde(default)]
    pub features: SpawndFeatures,
    /// HX6 finding 4: this host's recording probe; absent ⇒ not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_probe: Option<RecordingProbeEvidence>,
    pub checks: Readiness,
}
impl EvidenceReport {
    pub fn current(checks: Readiness, host_pty_budget: u32) -> io::Result<Self> {
        Ok(Self { version: 1, boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().to_owned(),
            checked_boottime_ms: boottime_ms()?, valid_for_ms: 1_000, host_pty_budget, landlock: None,
            features: DECODED_FEATURES, recording_probe: None, checks })
    }
    /// Finding 4: a passed probe of THIS boot under THIS Landlock ABI, and the
    /// build kill switch open. The only source of the `helper_recording` bit.
    pub fn recording_probe_passed(&self) -> bool {
        RECORDING_BUILD_ENABLED && self.recording_probe_matches()
    }
    fn recording_probe_matches(&self) -> bool {
        match (&self.recording_probe, &self.landlock) {
            (Some(probe), Some(landlock)) => probe.passed && probe.boot_id == self.boot_id
                && probe.landlock_abi == landlock.selected_abi,
            _ => false,
        }
    }
    /// Gateway gate for every `ipc::Opcode::WORK` op (H4/H5/H16).
    pub fn work_broker(&self) -> bool { self.features.contains(SpawndFeatures::WORK_BROKER) }
    /// Gateway gate for every `ipc::Opcode::HELPER` op (HX5); without it the
    /// gateway reports `helper_launcher_unavailable{spawnd_feature_missing}`.
    pub fn helper_calls(&self) -> bool { self.features.contains(SpawndFeatures::HELPER_CALLS) }
    /// HX6: Chromium proven to launch under the helper sandbox (recording gate):
    /// the bit AND the matching probe evidence (finding 4) AND the kill switch.
    pub fn helper_recording(&self) -> bool {
        self.helper_calls() && self.features.contains(SpawndFeatures::HELPER_RECORDING) && self.recording_probe_passed()
    }
    pub fn check_at(&self, boot_id: &str, now: u64) -> crate::Result<()> {
        if self.version != 1 || self.boot_id != boot_id || self.valid_for_ms == 0 || self.valid_for_ms > 1_000
            || now < self.checked_boottime_ms || now - self.checked_boottime_ms >= self.valid_for_ms
            || self.host_pty_budget == 0
            || (self.checks.trampoline_verified && !self.landlock.as_ref().is_some_and(LandlockEvidence::valid)) { return Err(ErrorCode::IsolationUnavailable); }
        self.checks.check()
    }
    pub fn publish(&self, gateway_gid: u32) -> io::Result<()> { atomic_json(Path::new(READINESS_PATH), self, gateway_gid, 0o640) }
    /// A may read this file or invoke --check. No booleans are accepted over IPC.
    pub fn read(gateway_gid: u32) -> io::Result<Self> { read_json(Path::new(READINESS_PATH), gateway_gid, 0o640) }
    pub fn check(&self) -> crate::Result<()> {
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|_| ErrorCode::IsolationUnavailable)?;
        self.check_at(boot.trim(), boottime_ms().map_err(|_| ErrorCode::IsolationUnavailable)?)
    }
}
pub fn boottime_ms() -> io::Result<u64> {
    let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0 { return Err(io::Error::last_os_error()); }
    Ok((value.tv_sec as u64).saturating_mul(1000) + value.tv_nsec as u64 / 1_000_000)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_rejects_stale_future_different_boot_and_missing_components() {
        let report = EvidenceReport { version: 1, boot_id: "boot".into(), checked_boottime_ms: 100,
            valid_for_ms: 1000, host_pty_budget: 100, landlock: Some(LandlockEvidence { detected_abi: 8, selected_abi: 7 }),
            features: DECODED_FEATURES, recording_probe: None, checks: Readiness {
                protocol: 1, registry_verified: true, accounts_locked: true, ssh_live_negative_checks: true,
                cron_at_acl_and_jobs_verified: true, external_iam_reservation_verified: true,
                trampoline_verified: true, landlock_self_test: true, namespace_self_test: true,
                egress_verified: true, dns_route_verified: true, storage_verified: true, resources_verified: true,
                startup_reconciled: true, process_scan_verified: true,
                egress_second_layer: crate::readiness::EgressSecondLayer::Unavailable,
            } };
        assert!(report.check_at("boot", 100).is_ok());
        let mut missing_abi = report.clone(); missing_abi.landlock = None;
        assert!(missing_abi.check_at("boot", 100).is_err());
        let mut invalid_abi = report.clone();
        invalid_abi.landlock = Some(LandlockEvidence { detected_abi: 8, selected_abi: 8 });
        assert!(invalid_abi.check_at("boot", 100).is_err());
        for (boot, now) in [("other", 101), ("boot", 99), ("boot", 1100)] { assert!(report.check_at(boot, now).is_err()); }
    }
    #[test]
    fn h4_feature_bits_advertised() {
        let report = EvidenceReport::current(Readiness::default(), 1).unwrap();
        assert!(report.work_broker(), "this spawnd decodes every Package H op");
        assert!(!report.helper_calls(), "helper_calls only once Core::refresh proves helpers can run");
        let value = serde_json::to_value(&report).unwrap();
        // The same bitset (and wire form) as `SpawnResponse::Reserved.features`.
        assert_eq!(value["features"], SpawndFeatures::WORK_BROKER.0);
        // A report from an older spawnd has no bits: the gateway must not send.
        let mut old = value.clone();
        old.as_object_mut().unwrap().remove("features");
        assert!(!serde_json::from_value::<EvidenceReport>(old).unwrap().work_broker());
        let mut off = value.clone();
        off["features"] = 0.into();
        assert!(!serde_json::from_value::<EvidenceReport>(off).unwrap().work_broker());
        // HX4 adds helper_calls to the same set; an unknown newer bit is ignored...
        let mut both = value.clone();
        both["features"] = SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS).0.into();
        let both = serde_json::from_value::<EvidenceReport>(both).unwrap();
        assert!(both.work_broker() && both.helper_calls());
        let mut newer = value.clone();
        newer["features"] = (SpawndFeatures::WORK_BROKER.0 | 1 << 9).into();
        let newer = serde_json::from_value::<EvidenceReport>(newer).unwrap();
        assert!(newer.work_broker() && !newer.helper_calls());
        // ...while the report itself stays strict.
        let mut extra = value;
        extra["work_broker"] = true.into();
        assert!(serde_json::from_value::<EvidenceReport>(extra).is_err());
        assert_eq!(duduclaw_isolation::ipc::Opcode::WORK.len(), 4);
        assert_eq!(duduclaw_isolation::ipc::Opcode::HELPER.len(), 4);
    }
    /// HX6 security fix round (finding 4): the recording bit is the result of
    /// THIS host's probe (this boot, this Landlock ABI) AND the build kill
    /// switch; a report without matching evidence never carries it, and the
    /// wire form is unchanged while no probe ran.
    #[test]
    fn hx6fix_recording_bit_requires_this_hosts_probe_and_the_kill_switch() {
        let mut report = EvidenceReport::current(Readiness::default(), 1).unwrap();
        report.landlock = Some(LandlockEvidence { detected_abi: 6, selected_abi: 6 });
        report.features = SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS).with(SpawndFeatures::HELPER_RECORDING);
        assert!(!report.helper_recording(), "a bit without probe evidence is not advertised");
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("recording_probe").is_none(), "wire form unchanged while no probe ran");
        let passed = RecordingProbeEvidence { boot_id: report.boot_id.clone(), landlock_abi: 6, passed: true };
        report.recording_probe = Some(passed.clone());
        assert_eq!(report.recording_probe_passed(), RECORDING_BUILD_ENABLED);
        assert_eq!(report.helper_recording(), RECORDING_BUILD_ENABLED);
        assert!(!RECORDING_BUILD_ENABLED, "the production gate stays closed until H15 acceptance");
        for evidence in [RecordingProbeEvidence { passed: false, ..passed.clone() },
            RecordingProbeEvidence { boot_id: "other-boot".into(), ..passed.clone() },
            RecordingProbeEvidence { landlock_abi: 4, ..passed.clone() }] {
            report.recording_probe = Some(evidence.clone());
            assert!(!report.recording_probe_matches(), "{evidence:?}");
            assert!(!report.helper_recording());
        }
        report.recording_probe = Some(passed);
        assert!(report.recording_probe_matches());
        report.landlock = None;
        assert!(!report.recording_probe_matches(), "no Landlock evidence, no recording");
        let back: EvidenceReport = serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(back.recording_probe, report.recording_probe);
    }
    /// HX6 security round 3 (N4): the gate is the published report's value,
    /// shared by clones (Core writes, LinuxHost reads) and readable while
    /// another thread holds any lock; closed on poison and by default.
    #[test]
    fn hx6r3_recording_gate_follows_published_report_without_lock() {
        let gate = RecordingGate::default();
        let reader = gate.clone();
        assert!(!reader.ready(), "closed until a report is published");
        let mut report = EvidenceReport::current(Readiness::default(), 1).unwrap();
        report.landlock = Some(LandlockEvidence { detected_abi: 6, selected_abi: 6 });
        report.features = SpawndFeatures::WORK_BROKER.with(SpawndFeatures::HELPER_CALLS).with(SpawndFeatures::HELPER_RECORDING);
        report.recording_probe = Some(RecordingProbeEvidence { boot_id: report.boot_id.clone(), landlock_abi: 6, passed: true });
        // A held view lock (the old `try_lock` contention) does not change the answer.
        let view = std::sync::Mutex::new(());
        let _held = view.lock().unwrap();
        gate.publish(&report, false);
        assert_eq!(reader.ready(), report.helper_recording());
        assert_eq!(reader.ready(), RECORDING_BUILD_ENABLED);
        gate.publish(&report, true);
        assert!(!reader.ready(), "a poisoned core closes the gate");
        report.recording_probe = None;
        gate.publish(&report, false);
        assert!(!reader.ready(), "no probe evidence, no recording");
    }
}

#[cfg(test)]
mod root_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    #[ignore = "iso_root_: disposable root Linux VM; writes a new directory below /root"]
    fn iso_root_run_journal_fsync_failure_poison_and_reopen() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let path = PathBuf::from(format!("/root/iso-b1-journal-{}", std::process::id()));
        fs::create_dir(&path).unwrap(); fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let run = RunId([0x41; 16]);
        let mut journal = RunJournal::open_at(&path).unwrap();
        journal.add(RunRecord { run, employee: EmployeeId([0x42; 16]), uid: 70000 }).unwrap();
        fs::create_dir(path.join("runs.next")).unwrap();
        assert!(journal.remove(run).is_err());
        fs::remove_dir(path.join("runs.next")).unwrap();
        assert!(journal.remove(run).is_err());
        let mut reopened = RunJournal::open_at(&path).unwrap();
        assert_eq!(reopened.records.len(), 1);
        reopened.remove(run).unwrap();
        assert!(RunJournal::open_at(&path).unwrap().records.is_empty());
        fs::remove_file(path.join("runs.json")).unwrap(); fs::remove_dir(path).unwrap();
    }
    #[test]
    #[ignore = "iso_root_: disposable root Linux VM; atomic evidence ownership/mode readback"]
    fn iso_root_readiness_report_atomic_mode_and_boot_validation() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let dir = PathBuf::from(format!("/root/iso-b1-evidence-{}", std::process::id()));
        fs::create_dir(&dir).unwrap(); fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("readiness.json");
        let report = EvidenceReport::current(Readiness::default(), 10).unwrap();
        atomic_json(&path, &report, 0, 0o640).unwrap();
        let read: EvidenceReport = read_json(&path, 0, 0o640).unwrap();
        assert_eq!(read.boot_id, report.boot_id); assert!(read.check().is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_json::<EvidenceReport>(&path, 0, 0o640).is_err());
        fs::remove_file(path).unwrap(); fs::remove_dir(dir).unwrap();
    }
}
