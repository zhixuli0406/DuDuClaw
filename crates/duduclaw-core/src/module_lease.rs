//! Internal module admission spike. No gateway entry point uses this API yet.
//! Stores must serialize the entire read/check/write transaction across handles.
use std::{collections::HashMap, marker::PhantomData, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};

mod sealed { pub trait Sealed {} }
pub trait Module: sealed::Sealed + Send + Sync + 'static { const ID: ModuleId; }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleId { OdooLivechat, Voice, Discovery, ComputerUse }
impl ModuleId {
    pub fn as_str(self) -> &'static str {
        match self { Self::OdooLivechat => "odoo_livechat", Self::Voice => "voice", Self::Discovery => "discovery", Self::ComputerUse => "computer_use" }
    }
}
macro_rules! modules {
    ($($name:ident),+) => { $(pub struct $name; impl sealed::Sealed for $name {} impl Module for $name { const ID: ModuleId = ModuleId::$name; })+ };
}
modules!(OdooLivechat, Voice, Discovery, ComputerUse);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleState { Active, Draining { deadline: i64 }, Disabled, Purging, Purged }
#[derive(Debug, Clone, Copy)]
pub struct ModuleSpec { pub id: ModuleId, pub display_name: &'static str, pub default_state: ModuleState }
// Config-dependent pilots start closed until the future gateway initialization resolves config.
pub static MODULE_SPECS: [ModuleSpec; 4] = [
    ModuleSpec { id: ModuleId::OdooLivechat, display_name: "Odoo 線上客服", default_state: ModuleState::Disabled },
    ModuleSpec { id: ModuleId::Voice, display_name: "語音", default_state: ModuleState::Disabled },
    ModuleSpec { id: ModuleId::Discovery, display_name: "Discovery", default_state: ModuleState::Disabled },
    ModuleSpec { id: ModuleId::ComputerUse, display_name: "電腦操作", default_state: ModuleState::Active },
];
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseKind {
    Rpc, Http, McpCall, LoopTick, ChannelHook, Cli, Work { work_id: String }, Cleanup,
    /// Q5: status/list/history reads remain available while disabled; never authorizes execution.
    ReadAccess,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason { Disabled, Draining, StoreUnreadable, NotInstalled, VersionMismatch, IllegalTransition, WorkNotFound }
#[derive(Debug, thiserror::Error)]
#[error("module {module:?} unavailable: {reason:?} ({state:?})")]
pub struct ModuleUnavailable { pub module: ModuleId, pub state: Option<ModuleState>, pub reason: UnavailableReason }

/// Persistence DTO, not an admission proof. Only Work slots are persisted.
#[derive(Clone)]
pub struct Inflight { pub slot: String, pub work_id: String, pub generation: u64, pub pid: u32, pub heartbeat_at: i64 }
#[derive(Clone)]
pub struct ModuleRecord {
    pub state: ModuleState, pub generation: u64, pub updated_by: String, pub updated_at: i64,
    pub inflight: Vec<Inflight>,
}
impl ModuleRecord {
    pub fn initial(id: ModuleId) -> Self {
        Self { state: MODULE_SPECS.iter().find(|s| s.id == id).unwrap().default_state,
            generation: 0, updated_by: "defaults".into(), updated_at: 0, inflight: vec![] }
    }
}
#[derive(Debug, thiserror::Error)]
#[error("module store unavailable: {0}")]
pub struct StoreError(pub String);
pub trait ModuleStore: Send + Sync {
    /// Must rollback on callback error and commit durably before returning success.
    /// Missing module rows in an existing store must return an error, never defaults.
    fn transaction(&self, id: ModuleId, action: &mut dyn FnMut(&mut ModuleRecord) -> Result<(), ModuleUnavailable>) -> Result<(), ModuleUnavailable>;
}
#[derive(Default)]
pub struct MemoryStore { records: Mutex<Option<HashMap<ModuleId, ModuleRecord>>> }
impl ModuleStore for MemoryStore {
    fn transaction(&self, id: ModuleId, action: &mut dyn FnMut(&mut ModuleRecord) -> Result<(), ModuleUnavailable>) -> Result<(), ModuleUnavailable> {
        let mut guard = self.records.lock().map_err(|_| unavailable(id, None, UnavailableReason::StoreUnreadable))?;
        let records = guard.get_or_insert_with(|| MODULE_SPECS.iter().map(|s| (s.id, ModuleRecord::initial(s.id))).collect());
        let mut row = records.get(&id).cloned().ok_or_else(|| unavailable(id, None, UnavailableReason::StoreUnreadable))?;
        action(&mut row)?;
        records.insert(id, row);
        Ok(())
    }
}
pub fn unavailable(module: ModuleId, state: Option<ModuleState>, reason: UnavailableReason) -> ModuleUnavailable {
    ModuleUnavailable { module, state, reason }
}
pub const SHORT_CALL_GRACE_SECONDS: i64 = 420;
fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }
fn dead(pid: u32) -> bool {
    #[cfg(unix)] {
        if pid == 0 || pid > i32::MAX as u32 { return false; }
        // Permission failures and PID reuse retain the slot conservatively.
        unsafe { libc::kill(pid as i32, 0) == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) }
    }
    #[cfg(not(unix))] { let _ = pid; false }
}
fn reconcile(row: &mut ModuleRecord, time: i64) {
    row.inflight.retain(|slot| !dead(slot.pid));
    if let ModuleState::Draining { deadline } = row.state {
        // Deadline is an admission cutoff, not proof that underlying work has stopped.
        // Keep live overdue work until its guard drops; future wiring handles needs_human.
        let _ = deadline;
        if row.inflight.is_empty() && time >= row.updated_at.saturating_add(SHORT_CALL_GRACE_SECONDS) {
            row.state = ModuleState::Disabled;
            row.generation += 1;
            row.updated_at = time;
            row.updated_by = "drain".into();
        }
    }
}
pub struct ModuleGate { store: Arc<dyn ModuleStore> }
impl ModuleGate {
    pub fn new(store: Arc<dyn ModuleStore>) -> Self { Self { store } }
    pub fn state<M: Module>(&self) -> Result<ModuleState, ModuleUnavailable> {
        let mut state = None;
        self.store.transaction(M::ID, &mut |row| { reconcile(row, now()); state = Some(row.state); Ok(()) })?;
        Ok(state.unwrap())
    }
    /// Admin-side primitive only; authorization belongs to future gateway wiring.
    pub fn transition<M: Module>(&self, target: ModuleState, actor: &str) -> Result<(), ModuleUnavailable> {
        self.store.transaction(M::ID, &mut |row| {
            let legal = matches!((row.state, target),
                (ModuleState::Active, ModuleState::Draining { .. }) |
                (ModuleState::Disabled, ModuleState::Active) |
                (ModuleState::Disabled, ModuleState::Purging) |
                (ModuleState::Purging, ModuleState::Purged));
            if !legal || matches!(target, ModuleState::Draining { deadline } if deadline < now().saturating_add(SHORT_CALL_GRACE_SECONDS)) {
                return Err(unavailable(M::ID, Some(row.state), UnavailableReason::IllegalTransition));
            }
            row.state = target; row.generation += 1; row.updated_at = now(); row.updated_by = actor.into();
            Ok(())
        })
    }
    pub fn acquire<M: Module>(&self, kind: LeaseKind) -> Result<ModuleLease<M>, ModuleUnavailable> { self.admit(kind, false) }
    pub fn resume<M: Module>(&self, work_id: &str) -> Result<ModuleLease<M>, ModuleUnavailable> {
        self.admit(LeaseKind::Work { work_id: work_id.into() }, true)
    }
    fn admit<M: Module>(&self, kind: LeaseKind, resume: bool) -> Result<ModuleLease<M>, ModuleUnavailable> {
        let slot = uuid::Uuid::new_v4().to_string();
        let mut generation = 0;
        self.store.transaction(M::ID, &mut |row| {
            let time = now(); reconcile(row, time);
            let exceptional = matches!(kind, LeaseKind::Cleanup | LeaseKind::ReadAccess);
            let resuming = resume && matches!(row.state, ModuleState::Active | ModuleState::Draining { deadline: _ });
            if !exceptional && row.state != ModuleState::Active && !resuming {
                return Err(unavailable(M::ID, Some(row.state), if matches!(row.state, ModuleState::Draining { .. }) { UnavailableReason::Draining } else { UnavailableReason::Disabled }));
            }
            if let LeaseKind::Work { ref work_id } = kind {
                if resume && (matches!(row.state, ModuleState::Draining { deadline } if time >= deadline) || !row.inflight.iter().any(|s| s.work_id == *work_id)) {
                    return Err(unavailable(M::ID, Some(row.state), UnavailableReason::WorkNotFound));
                }
                if work_id.is_empty() || (!resume && row.inflight.iter().any(|s| s.work_id == *work_id)) {
                    return Err(unavailable(M::ID, Some(row.state), UnavailableReason::IllegalTransition));
                }
                row.inflight.push(Inflight { slot: slot.clone(), work_id: work_id.clone(), generation: row.generation, pid: std::process::id(), heartbeat_at: time });
            }
            generation = row.generation; Ok(())
        })?;
        Ok(ModuleLease { generation, kind, slot, store: self.store.clone(), _module: PhantomData })
    }
}
/// Non-cloneable admission proof. ReadAccess and Cleanup proofs only authorize their own kind.
#[must_use]
pub struct ModuleLease<M: Module> { generation: u64, kind: LeaseKind, slot: String, store: Arc<dyn ModuleStore>, _module: PhantomData<M> }
impl<M: Module> ModuleLease<M> {
    pub fn generation(&self) -> u64 { self.generation }
    pub fn kind(&self) -> &LeaseKind { &self.kind }
}
impl<M: Module> Drop for ModuleLease<M> {
    fn drop(&mut self) {
        if let Err(error) = self.store.transaction(M::ID, &mut |row| {
            row.inflight.retain(|s| s.slot != self.slot); reconcile(row, now()); Ok(())
        }) { tracing::error!(%error, "module lease release failed; slot retained fail closed"); }
    }
}
#[cfg(test)]
mod tests;
