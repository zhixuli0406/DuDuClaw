//! R8d: root writes of the host entitlements in
//! `/var/lib/duduclaw-spawnd/publication.json` (`installed::HostPublication`).
//!
//! Trust: the gateway account is the authority for entitlements exactly as it
//! is for account allocation (same peer-checked allocation socket, same
//! lifecycle worker). Root enforces signed-policy membership, a Ready
//! allocation (`allocation::Allocator`), the shape rules and capacity, and it
//! validates the WHOLE candidate publication with the production check before
//! anything is written. A refused request writes nothing.
//!
//! Three writers, all on spawnd's lifecycle thread:
//! - [`entitle`] (`Action::Entitle`): replace this employee's one record;
//!   identical record ⇒ no write.
//! - [`revoke`] (`Disable` / `Retire`, before their state change): drop the
//!   employee's record; absent ⇒ no write (crash-resume safe).
//! - [`roll_over`] (`InstalledPublication::load`): a publication of another
//!   generation keeps, per record, only the profiles the new signed policy
//!   still has; empty records are dropped, a `max_turns` that no longer
//!   narrows every signed range validly is dropped, the generation stamped.
use std::io;
use duduclaw_isolation::{egress::ProfileId, ids::{EmployeeId, Sha256Digest}};
use duduclaw_migration::allocation::Entitlement;
use crate::{installed::{EntitlementRecord, GatewayPublication, HostPublication, RuntimeRecord},
    policy::{effective_max_turns, EmployeeEntitlement, RootPolicy, TURN_LIMIT_CEILING}};

/// Why root did not change the host entitlements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A profile is not in the committed signed policy. Nothing written.
    NotSigned,
    /// Empty / over 128 / duplicated profiles, or an invalid `max_turns`. Nothing written.
    Invalid,
    /// Over the entry cap or the gateway projection's size cap. Nothing written.
    Capacity,
    /// A source could not be read or the publication is not in force; or a
    /// write failed and a re-read proved the old file is still in force.
    Unavailable,
    /// A write failed and the on-disk state is unknown (poison rule).
    Unknown,
}
/// Same bound as `RootPolicy::from_verified_publication`.
pub const MAX_ENTITLEMENTS: usize = 30_000;
const MAX_PROFILES: usize = 128;
/// A boot id as `/proc/sys/kernel/random/boot_id` gives it (36 characters):
/// the projection is sized with the longest values its fields can take.
const BOOT_ID_SHAPE: &str = "00000000-0000-0000-0000-000000000000";

/// The root host publication store (production: `installed::InstalledEntitlements`).
pub trait Entitlements {
    /// The committed generation and its verified signed runtimes.
    fn signed(&mut self) -> io::Result<(Sha256Digest, Vec<RuntimeRecord>)>;
    /// The host publication as it is on disk (strict structural read).
    fn read(&mut self) -> io::Result<HostPublication>;
    /// Atomically replace `previous` (just read) with `next`.
    fn write(&mut self, previous: &HostPublication, next: &HostPublication) -> Result<(), Refusal>;
}

/// The production publication check (`GatewayPublication::policy`, which
/// `load` runs): runtimes and every entitlement as one whole.
pub(crate) fn admitted(runtimes: &[RuntimeRecord], entitlements: &[EntitlementRecord]) -> crate::Result<RootPolicy> {
    RootPolicy::from_verified_publication(runtimes.iter().map(RuntimeRecord::to_runtime).collect(),
        entitlements.iter().map(|e| EmployeeEntitlement { employee: e.employee, profiles: e.profiles.clone(), max_turns: e.max_turns })
            .collect())
}

/// The gateway projection `load` would publish, with the longest boot id and
/// timestamp it can carry.
pub(crate) fn projection(generation: Sha256Digest, runtimes: &[RuntimeRecord], entitlements: &[EntitlementRecord]) -> GatewayPublication {
    GatewayPublication { version: 1, generation, boot_id: BOOT_ID_SHAPE.into(), checked_boottime_ms: u64::MAX, valid_for_ms: 1000,
        runtimes: runtimes.to_vec(), entitlements: entitlements.to_vec() }
}

/// Whether `entitlements` would be in force: the entry cap, the production
/// check, and the projection within `host_state::MAX_BYTES`.
fn in_force(generation: Sha256Digest, runtimes: &[RuntimeRecord], entitlements: &[EntitlementRecord]) -> Result<(), Refusal> {
    if entitlements.len() > MAX_ENTITLEMENTS { return Err(Refusal::Capacity); }
    admitted(runtimes, entitlements).map_err(|_| Refusal::Unavailable)?;
    let bytes = serde_json::to_vec(&projection(generation, runtimes, entitlements)).map_err(|_| Refusal::Unavailable)?;
    if bytes.len() as u64 > crate::host_state::MAX_BYTES { return Err(Refusal::Capacity); }
    Ok(())
}

/// The request's own shape (before any read).
fn shape(wanted: &Entitlement) -> Result<(), Refusal> {
    let profiles = &wanted.profiles;
    if profiles.is_empty() || profiles.len() > MAX_PROFILES
        || profiles.iter().enumerate().any(|(index, profile)| profiles.iter().take(index).any(|earlier| earlier == profile))
        || wanted.max_turns.is_some_and(|cap| !(1..=TURN_LIMIT_CEILING).contains(&cap)) {
        return Err(Refusal::Invalid);
    }
    Ok(())
}

/// `cap` narrows validly: within 1..=1000 and, for every entitled signed step
/// range, not below its minimum (`effective_max_turns` would refuse every run).
fn narrows(cap: u16, profiles: &[ProfileId], runtimes: &[RuntimeRecord]) -> bool {
    (1..=TURN_LIMIT_CEILING).contains(&cap) && runtimes.iter().filter(|runtime| profiles.contains(&runtime.profile))
        .all(|runtime| runtime.turn_limit.is_none() || effective_max_turns(runtime.turn_limit, Some(cap), None).is_ok())
}

/// After a write: the file must now hold exactly `next`; anything else is unknown.
fn read_back(store: &mut dyn Entitlements, next: &HostPublication) -> Result<(), Refusal> {
    if store.read().ok().as_ref() != Some(next) { return Err(Refusal::Unknown); }
    Ok(())
}

/// `Entitle` for `employee` (the caller proved its allocation is Ready).
/// Returns what is now in force for it.
pub fn entitle(store: &mut dyn Entitlements, employee: EmployeeId, wanted: &Entitlement) -> Result<Entitlement, Refusal> {
    shape(wanted)?;
    let (generation, runtimes) = store.signed().map_err(|_| Refusal::Unavailable)?;
    if wanted.profiles.iter().any(|profile| !runtimes.iter().any(|runtime| runtime.profile == *profile)) { return Err(Refusal::NotSigned); }
    if wanted.max_turns.is_some_and(|cap| !narrows(cap, &wanted.profiles, &runtimes)) { return Err(Refusal::Invalid); }
    let current = store.read().map_err(|_| Refusal::Unavailable)?;
    // Not rolled over yet: `load` migrates it within a refresh; retry later.
    if current.version != 1 || current.generation != generation { return Err(Refusal::Unavailable); }
    let record = EntitlementRecord { employee, profiles: wanted.profiles.clone(), max_turns: wanted.max_turns };
    let unchanged = current.entitlements.contains(&record);
    let mut next = current.clone();
    match next.entitlements.iter().position(|held| held.employee == employee) {
        Some(index) => next.entitlements[index] = record,
        None => next.entitlements.push(record),
    }
    in_force(generation, &runtimes, &next.entitlements)?;
    if !unchanged {
        store.write(&current, &next)?;
        read_back(store, &next)?;
    }
    let held = next.entitlements.iter().find(|held| held.employee == employee).ok_or(Refusal::Unknown)?;
    Ok(Entitlement { profiles: held.profiles.clone(), max_turns: held.max_turns })
}

/// `Disable` / `Retire`: the employee holds no entitlement afterwards.
pub fn revoke(store: &mut dyn Entitlements, employee: EmployeeId) -> Result<(), Refusal> {
    let current = store.read().map_err(|_| Refusal::Unavailable)?;
    if current.version != 1 { return Err(Refusal::Unavailable); }
    if !current.entitlements.iter().any(|held| held.employee == employee) { return Ok(()); }
    let mut next = current.clone();
    next.entitlements.retain(|held| held.employee != employee);
    store.write(&current, &next)?;
    read_back(store, &next)
}

/// The rollover rule (pure): see the module documentation.
pub fn rolled_over(host: &HostPublication, generation: Sha256Digest, runtimes: &[RuntimeRecord]) -> HostPublication {
    let entitlements = host.entitlements.iter().filter_map(|record| {
        let profiles: Vec<ProfileId> = record.profiles.iter().copied()
            .filter(|profile| runtimes.iter().any(|runtime| runtime.profile == *profile)).collect();
        if profiles.is_empty() { return None; }
        let max_turns = record.max_turns.filter(|cap| narrows(*cap, &profiles, runtimes));
        Some(EntitlementRecord { employee: record.employee, profiles, max_turns })
    }).collect();
    HostPublication { version: host.version, generation, entitlements }
}

/// Roll `host` over to `generation` and write it. Only a version-1
/// publication is migrated; any failure refuses (the caller fails closed).
pub fn roll_over(store: &mut dyn Entitlements, host: &HostPublication, generation: Sha256Digest, runtimes: &[RuntimeRecord])
    -> Result<HostPublication, Refusal> {
    if host.version != 1 { return Err(Refusal::Unavailable); }
    let next = rolled_over(host, generation, runtimes);
    store.write(host, &next)?;
    read_back(store, &next)?;
    Ok(next)
}

/// In-memory store for unit tests (counts writes; injectable write failure).
#[cfg(test)]
pub(crate) struct Memory { pub(crate) publication: HostPublication, pub(crate) generation: Sha256Digest,
    pub(crate) runtimes: Vec<RuntimeRecord>, pub(crate) writes: usize, pub(crate) fail: Option<Refusal> }
#[cfg(test)]
impl Memory {
    /// An installed host: the signed `runtimes`, an empty publication of the same generation.
    pub(crate) fn new(runtimes: Vec<RuntimeRecord>) -> Self {
        let generation = Sha256Digest([9; 32]);
        Self { publication: HostPublication { version: 1, generation, entitlements: vec![] }, generation, runtimes, writes: 0, fail: None }
    }
    pub(crate) fn held(&self, employee: EmployeeId) -> Option<&EntitlementRecord> {
        self.publication.entitlements.iter().find(|held| held.employee == employee)
    }
}
#[cfg(test)]
impl Entitlements for Memory {
    fn signed(&mut self) -> io::Result<(Sha256Digest, Vec<RuntimeRecord>)> { Ok((self.generation, self.runtimes.clone())) }
    fn read(&mut self) -> io::Result<HostPublication> { Ok(self.publication.clone()) }
    fn write(&mut self, previous: &HostPublication, next: &HostPublication) -> Result<(), Refusal> {
        assert_eq!(*previous, self.publication, "a write replaces what was just read");
        if let Some(refusal) = self.fail { return Err(refusal); }
        self.writes += 1;
        self.publication = next.clone();
        Ok(())
    }
}
/// Signed runtime records that pass the production check.
#[cfg(test)]
pub(crate) mod fixture {
    use duduclaw_isolation::{bounded::Text, egress::ProfileId, execution::{DiskQuota, NetworkPolicy, ResourceLimits, RunTimeouts, RuntimeKind},
        ids::Sha256Digest, ipc::QueueClass};
    use crate::{installed::{ExecutableRecord, RuntimeRecord}, policy::TurnLimit};
    pub(crate) fn profile(byte: u8) -> ProfileId { ProfileId(Sha256Digest([byte; 32])) }
    pub(crate) fn runtime(byte: u8, kind: RuntimeKind) -> RuntimeRecord {
        RuntimeRecord { runtime: kind, profile: profile(byte),
            executable: ExecutableRecord::Elf { entry: Text::new("codex".into()).unwrap(), digest: Sha256Digest([1; 32]) },
            pinned_version: Text::new("1.2.3".into()).unwrap(), fixed_args: vec![],
            resources: ResourceLimits { memory_bytes: 1024 * 1024, cpu_quota_percent: 100, tasks: 16, open_files: 4096, ptys: 16,
                disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 }, io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024,
                io_read_ops_per_sec: 10, io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096,
                memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1000, startup_ms: 1000, runtime_ms: 1000, pty_idle_ms: None, termination_grace_ms: 100 },
            network: NetworkPolicy::Offline, mcp: false, pty_allowed: false, queue_class: QueueClass::Background, mcp_acceptance: None,
            turn_limit: None }
    }
    /// A policy v3 Claude MCP record with a signed step range.
    pub(crate) fn ranged(byte: u8, limit: TurnLimit) -> RuntimeRecord {
        let mut record = runtime(byte, RuntimeKind::Claude);
        record.mcp = true;
        record.fixed_args = crate::gateway_plan::claude_ranged_arguments("model").unwrap().into_iter()
            .map(|arg| Text::new(arg).unwrap()).collect();
        record.turn_limit = Some(limit);
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::fixture::{profile, ranged, runtime};
    use duduclaw_isolation::execution::RuntimeKind;
    use crate::policy::TurnLimit;
    fn employee(index: u16) -> EmployeeId {
        let mut id = [0x11; 16];
        let [high, low] = index.to_be_bytes();
        id[0] = high; id[1] = low;
        EmployeeId(id)
    }
    fn wanted(profiles: &[u8], max_turns: Option<u16>) -> Entitlement {
        Entitlement { profiles: profiles.iter().map(|byte| profile(*byte)).collect(), max_turns }
    }
    fn store() -> Memory {
        Memory::new(vec![runtime(1, RuntimeKind::Codex), runtime(2, RuntimeKind::OpenAiCompat),
            ranged(3, TurnLimit { min: 5, max: 200, default: 50 })])
    }
    /// What `load` runs on the written publication (minus the live updater):
    /// the projection's production check, and its size cap.
    fn loads(memory: &Memory) -> bool {
        let report = projection(memory.publication.generation, &memory.runtimes, &memory.publication.entitlements);
        memory.publication.generation == memory.generation && report.policy().is_ok()
            && serde_json::to_vec(&report).unwrap().len() as u64 <= crate::host_state::MAX_BYTES
    }

    #[test]
    fn entitle_writes_exactly_one_record_that_loads() {
        let mut memory = store();
        let granted = entitle(&mut memory, employee(1), &wanted(&[1, 3], Some(30))).unwrap();
        assert_eq!(granted, wanted(&[1, 3], Some(30)));
        assert_eq!(memory.writes, 1);
        assert_eq!(memory.publication.entitlements.len(), 1);
        assert_eq!(memory.held(employee(1)).unwrap().profiles, vec![profile(1), profile(3)]);
        assert!(loads(&memory));
        // A second employee is a second record; replacing keeps one per employee.
        entitle(&mut memory, employee(2), &wanted(&[2], None)).unwrap();
        entitle(&mut memory, employee(1), &wanted(&[2], None)).unwrap();
        assert_eq!((memory.writes, memory.publication.entitlements.len()), (3, 2));
        assert_eq!(memory.held(employee(1)).unwrap().profiles, vec![profile(2)]);
        assert!(loads(&memory));
    }

    #[test]
    fn identical_entitle_performs_no_write() {
        let mut memory = store();
        entitle(&mut memory, employee(1), &wanted(&[1], None)).unwrap();
        let before = memory.publication.clone();
        for _ in 0..3 {
            assert_eq!(entitle(&mut memory, employee(1), &wanted(&[1], None)).unwrap(), wanted(&[1], None));
        }
        assert_eq!(memory.writes, 1, "identical record: no write");
        assert_eq!(memory.publication, before);
    }

    #[test]
    fn entitle_refusals_leave_the_file_unchanged() {
        let cases: Vec<(Entitlement, Refusal)> = vec![
            (wanted(&[9], None), Refusal::NotSigned),
            (wanted(&[1, 9], None), Refusal::NotSigned),
            (wanted(&[], None), Refusal::Invalid),
            (wanted(&[1, 1], None), Refusal::Invalid),
            (wanted(&[1], Some(0)), Refusal::Invalid),
            (wanted(&[1], Some(1001)), Refusal::Invalid),
            // Below the signed minimum (5) of an entitled ranged profile.
            (wanted(&[3], Some(4)), Refusal::Invalid),
            (Entitlement { profiles: (0..=128u8).map(profile).collect(), max_turns: None }, Refusal::Invalid),
        ];
        for (request, refusal) in cases {
            let mut memory = store();
            entitle(&mut memory, employee(1), &wanted(&[2], None)).unwrap();
            let before = memory.publication.clone();
            assert_eq!(entitle(&mut memory, employee(1), &request).unwrap_err(), refusal, "{request:?}");
            assert_eq!((memory.writes, &memory.publication), (1, &before), "{request:?}");
        }
        // A cap that narrows a range validly is accepted.
        assert!(entitle(&mut store(), employee(1), &wanted(&[3], Some(5))).is_ok());
    }

    #[test]
    fn entitle_refuses_a_publication_not_in_force() {
        // Another generation (not rolled over yet) or a broken existing record.
        let mut memory = store();
        memory.publication.generation = Sha256Digest([8; 32]);
        assert_eq!(entitle(&mut memory, employee(1), &wanted(&[1], None)).unwrap_err(), Refusal::Unavailable);
        let mut memory = store();
        memory.publication.entitlements.push(EntitlementRecord { employee: employee(7), profiles: vec![profile(9)], max_turns: None });
        assert_eq!(entitle(&mut memory, employee(1), &wanted(&[1], None)).unwrap_err(), Refusal::Unavailable);
        assert_eq!(memory.writes, 0);
    }

    #[test]
    fn entitle_beyond_the_projection_cap_is_refused_with_capacity() {
        let mut memory = store();
        let mut index = 0u16;
        let refused = loop {
            match entitle(&mut memory, employee(index), &wanted(&[1], None)) {
                Ok(_) => index += 1,
                Err(refusal) => break refusal,
            }
            assert!(index < 2000, "the 64 KiB projection must fill up");
        };
        assert_eq!(refused, Refusal::Capacity);
        assert_eq!(memory.writes, usize::from(index), "the refused candidate wrote nothing");
        assert_eq!(memory.publication.entitlements.len(), usize::from(index));
        assert!(loads(&memory), "what is on disk still loads");
        // An employee already entitled can still be re-entitled identically.
        assert!(entitle(&mut memory, employee(0), &wanted(&[1], None)).is_ok());
    }

    #[test]
    fn record_wire_cost_is_bounded() {
        // The worst-case shapes the capacity statement is derived from:
        // envelope 300 bytes, 221 bytes per one-profile record (+1 separator).
        let empty = projection(Sha256Digest([255; 32]), &[], &[]);
        assert_eq!(serde_json::to_vec(&empty).unwrap().len(), 300);
        let record = EntitlementRecord { employee: EmployeeId([255; 16]), profiles: vec![ProfileId(Sha256Digest([255; 32]))], max_turns: None };
        assert_eq!(serde_json::to_vec(&record).unwrap().len(), 221);
    }

    #[test]
    fn revoke_removes_the_record_and_is_idempotent() {
        let mut memory = store();
        entitle(&mut memory, employee(1), &wanted(&[1], None)).unwrap();
        entitle(&mut memory, employee(2), &wanted(&[2], None)).unwrap();
        revoke(&mut memory, employee(1)).unwrap();
        assert!(memory.held(employee(1)).is_none() && memory.held(employee(2)).is_some());
        let writes = memory.writes;
        revoke(&mut memory, employee(1)).unwrap();
        revoke(&mut memory, employee(3)).unwrap();
        assert_eq!(memory.writes, writes, "absent: no write");
        // A failed write that left the old file in force is reported, not poisoned.
        memory.fail = Some(Refusal::Unavailable);
        assert_eq!(revoke(&mut memory, employee(2)).unwrap_err(), Refusal::Unavailable);
        assert!(memory.held(employee(2)).is_some());
        memory.fail = Some(Refusal::Unknown);
        assert_eq!(revoke(&mut memory, employee(2)).unwrap_err(), Refusal::Unknown);
    }

    #[test]
    fn rollover_filters_profiles_drops_empty_records_and_stamps_the_generation() {
        let old = Sha256Digest([8; 32]);
        let host = HostPublication { version: 1, generation: old, entitlements: vec![
            EntitlementRecord { employee: employee(1), profiles: vec![profile(1), profile(7)], max_turns: None },
            EntitlementRecord { employee: employee(2), profiles: vec![profile(7)], max_turns: None },
            // 30 still narrows the new range of profile 3; 4 is below its minimum.
            EntitlementRecord { employee: employee(3), profiles: vec![profile(3)], max_turns: Some(30) },
            EntitlementRecord { employee: employee(4), profiles: vec![profile(3), profile(2)], max_turns: Some(4) },
        ] };
        let mut memory = store();
        memory.publication = host.clone();
        let (generation, runtimes) = (memory.generation, memory.runtimes.clone());
        let next = roll_over(&mut memory, &host, generation, &runtimes).unwrap();
        assert_eq!(next.generation, memory.generation);
        assert_eq!(next.entitlements, vec![
            EntitlementRecord { employee: employee(1), profiles: vec![profile(1)], max_turns: None },
            EntitlementRecord { employee: employee(3), profiles: vec![profile(3)], max_turns: Some(30) },
            EntitlementRecord { employee: employee(4), profiles: vec![profile(3), profile(2)], max_turns: None },
        ]);
        assert_eq!((memory.writes, &memory.publication), (1, &next));
        assert!(loads(&memory), "the rolled-over publication reloads");
    }

    #[test]
    fn rollover_refuses_wrong_version_and_failed_write() {
        let host = HostPublication { version: 2, generation: Sha256Digest([8; 32]), entitlements: vec![] };
        let mut memory = store();
        memory.publication = host.clone();
        let (generation, runtimes) = (memory.generation, memory.runtimes.clone());
        assert_eq!(roll_over(&mut memory, &host, generation, &runtimes).unwrap_err(), Refusal::Unavailable);
        assert_eq!(memory.writes, 0);
        let host = HostPublication { version: 1, generation: Sha256Digest([8; 32]), entitlements: vec![] };
        for refusal in [Refusal::Unavailable, Refusal::Unknown] {
            let mut memory = store();
            memory.publication = host.clone(); memory.fail = Some(refusal);
            assert_eq!(roll_over(&mut memory, &host, generation, &runtimes).unwrap_err(), refusal);
            assert_eq!(memory.publication, host, "nothing migrated in memory either");
        }
        // Unknown fields are structural garbage: refused by the strict reader, never migrated.
        let value = serde_json::json!({"version": 1, "generation": Sha256Digest([8; 32]), "entitlements": [], "next": true});
        assert!(serde_json::from_value::<HostPublication>(value).is_err());
    }
}
