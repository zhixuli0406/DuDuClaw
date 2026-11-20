//! Confirmed allocation for a freshly configured employee. Imported settings
//! alone never create an enabled product employee or bypass normal product writes.
use super::*;
use duduclaw_migration::allocation::{self, Action, Answer, Entitlement, ErrorCode as Code, Receipt, Request, Response};
use duduclaw_migration::confirmation::EffectGate;
use duduclaw_migration::ids::RequestId;
use crate::isolation_entitlement::{Desired, DesiredError};
use duduclaw_isolation::capability::EmployeeBinding;
use crate::isolation_lifecycle::{self as lifecycle, LifecycleError};
use std::sync::Arc;

/// The root end of the schema-3 `Allocate` (R7d): the raw reply, which
/// [`allocate_with`] checks. Tests inject a fake.
pub(crate) trait AllocationRoot: Send + Sync {
    fn round_trip(&self, request:&Request)->std::result::Result<Response,()>;
}
/// Production: spawnd's allocation socket.
pub(crate) struct SpawndAllocationRoot;
impl AllocationRoot for SpawndAllocationRoot {
    fn round_trip(&self, request:&Request)->std::result::Result<Response,()> {
        #[cfg(target_os="linux")]{ allocation::transport::round_trip(request).map_err(|_|()) }
        #[cfg(not(target_os="linux"))]{ let _=request; Err(()) }
    }
}
/// R7d: one `Allocate` (schema 3) of `item`. Ready (a receipt checked by
/// `check_response`): the receipt. `Pending`: the root keeps the reservation
/// resumable, so the SAME request must be sent again later: `Err(Pending)`,
/// which never compensates and is waited for (bounded). `PendingLimit`
/// (nothing reserved): `Err(PendingLimit)`, the same retry-safe outcome but
/// never waited for in process. `NotReady` (round 1): spawnd answers it to a
/// schema-3 `Allocate` only before any journal entry or registry row is
/// created or changed — start-up reconciliation not latched
/// (`spawnd::host::Core::allocation`) or a run live on the host
/// (`spawnd::allocation::Allocator::handle`, the host gate) — and it clears
/// within seconds: `Err(Pending)`, waited for like `pending`. Everything else
/// — an older-schema or garbled reply, an unknown kind, a transport failure,
/// any other refusal (a tombstoned employee's included) — is the existing
/// hard failure. Only the stage step calls this; `NotReady` from
/// ReadBack/Disable/Retire (`exchange`, schema 1) keeps its meaning.
pub(crate) fn allocate_with(root:&dyn AllocationRoot,item:&ConfirmationItem)->Result<Receipt> {
    let request=Request{schema:allocation::SCHEMA_V3,nonce:RequestId(*uuid::Uuid::new_v4().as_bytes()),action:Action::Allocate,item:item.clone(),approver:None,
        entitlement:None};
    let response=root.round_trip(&request).map_err(|_|Error::Apply)?;
    match allocation::check_response(&request,response).map_err(|_|Error::Apply)? {
        Answer::Reply(reply)=>Ok(reply.receipt),
        Answer::Refused(Code::Pending|Code::NotReady)=>Err(Error::Pending),
        Answer::Refused(Code::PendingLimit)=>Err(Error::PendingLimit),
        Answer::Refused(_)=>Err(Error::Apply),
    }
}

/// R8d D2: one `Entitle` (schema 4) of `item`'s Ready employee with
/// `desired`, started only inside a permit of `gate` (closed ⇒ nothing is
/// sent: `Err(Pending)`, the step stays resumable). Success requires root's
/// answer to equal `desired` exactly. A transient root answer
/// (`isolation_unavailable` / `not_ready`, e.g. a publication not rolled over
/// yet) or a transport failure is `Err(Pending)`: the allocation already
/// exists, so a retry repeats Allocate (replayed) and Entitle (idempotent)
/// rather than compensating. Every other refusal or a mismatch is a failure.
pub(crate) fn entitle_with(root:&dyn AllocationRoot,gate:&dyn EffectGate,item:&ConfirmationItem,desired:&Entitlement)->Result<()> {
    let Some(_running)=gate.enter() else { return Err(Error::Pending); };
    let request=Request{schema:allocation::SCHEMA_V4,nonce:RequestId(*uuid::Uuid::new_v4().as_bytes()),action:Action::Entitle,item:item.clone(),
        approver:None,entitlement:Some(desired.clone())};
    let response=root.round_trip(&request).map_err(|_|Error::Pending)?;
    match allocation::check_response(&request,response).map_err(|_|Error::Apply)? {
        Answer::Reply(reply) if reply.entitlement.as_ref()==Some(desired)=>Ok(()),
        Answer::Reply(_)=>Err(Error::Apply),
        Answer::Refused(Code::IsolationUnavailable|Code::NotReady)=>Err(Error::Pending),
        Answer::Refused(_)=>Err(Error::Apply),
    }
}

/// R8d D1: the entitlement an approved settings document needs, against the
/// fresh signed runtimes. Tests inject a fake.
pub(crate) trait EntitlementPlanner: Send + Sync {
    fn desired(&self,settings:&serde_json::Value)->std::result::Result<Desired,DesiredError>;
}
/// Production: the gateway projection spawnd republishes every second.
pub(crate) struct InstalledPlanner;
impl EntitlementPlanner for InstalledPlanner {
    fn desired(&self,settings:&serde_json::Value)->std::result::Result<Desired,DesiredError> {
        #[cfg(target_os="linux")]{
            let publication=duduclaw_spawnd::installed::GatewayPublication::read(nix::unistd::getegid().as_raw())
                .map_err(|_|DesiredError::Unavailable)?;
            let runtimes:Vec<_>=publication.runtimes.iter().map(duduclaw_spawnd::installed::RuntimeRecord::to_runtime).collect();
            crate::isolation_entitlement::desired(&runtimes,settings)
        }
        #[cfg(not(target_os="linux"))]{ let _=settings; Err(DesiredError::Unavailable) }
    }
}
/// R8d D1: the desired entitlement of `item`'s APPROVED settings. A primary
/// runtime without a signed profile fails the item (`Denied`, logged with the
/// stable code `runtime_not_available_in_isolation`) before anything is
/// allocated; an unreadable projection is retry-safe (`Pending`).
pub(crate) fn planned(planner:&dyn EntitlementPlanner,item:&ConfirmationItem)->Result<Desired> {
    let settings:serde_json::Value=serde_json::from_str(text(item,"settings")?).map_err(|_|Error::Invalid)?;
    let label=allocation::specification(item).map(|spec|spec.label).unwrap_or_default();
    match planner.desired(&settings) {
        Ok(desired)=>{
            if !desired.skipped_fallbacks.is_empty() {
                tracing::warn!(label=%label,skipped=?desired.skipped_fallbacks,
                    "fallback runtime has no signed isolation profile; skipped (not fatal), the employee runs its primary runtime only");
            }
            Ok(desired)
        }
        Err(DesiredError::RuntimeNotInIsolation{runtime})=>{
            tracing::error!(label=%label,runtime=%runtime,code=crate::isolation_entitlement::RUNTIME_NOT_IN_ISOLATION,
                "the employee's runtime is not available in isolation mode (no signed profile); item failed before any account was created \
                 (fallback runtimes without a signed profile are skipped, not fatal)");
            Err(Error::Denied)
        }
        Err(DesiredError::Unavailable)=>Err(Error::Pending),
    }
}
/// R8d D1–D2, the stage's root effects: the desired set first (no account
/// for an unavailable runtime), then Allocate, then Entitle of the Ready
/// employee. A crash anywhere repeats both idempotently on resume.
pub(crate) fn allocate_entitled(root:&dyn AllocationRoot,planner:&dyn EntitlementPlanner,gate:&dyn EffectGate,item:&ConfirmationItem)->Result<Receipt> {
    let desired=planned(planner,item)?;
    let receipt=allocate_with(root,item)?;
    entitle_with(root,gate,item,&desired.entitlement)?;
    Ok(receipt)
}

pub(crate) struct EmployeeAuthority { home:PathBuf, live:Arc<LiveAuthority>, agent_cap:usize, step_up_verified_at:Option<u64>, root:Arc<dyn AllocationRoot>,
    planner:Arc<dyn EntitlementPlanner> }
impl EmployeeAuthority {
    pub(crate) fn new(home:PathBuf,live:Arc<LiveAuthority>)->Self{Self{home,live,agent_cap:0,step_up_verified_at:None,root:Arc::new(SpawndAllocationRoot),
        planner:Arc::new(InstalledPlanner)}}
    /// F6fb item 8: the licence agent cap (`MethodHandler::effective_agent_cap`,
    /// 0 = unlimited) that an adoption's approval and apply enforce.
    pub(crate) fn with_agent_cap(mut self,agent_cap:usize)->Self{self.agent_cap=agent_cap;self}
    /// Security round 3 N2: `verified_at` of the migration step-up proof the
    /// confirmation RPC read for this call (`None`: none). An adoption card's
    /// F6c2 decision computes `fresh_step_up` from it (`step_up_fresh`), as
    /// the restore decide and the retire card do.
    pub(crate) fn with_step_up(mut self,verified_at:Option<u64>)->Self{self.step_up_verified_at=verified_at;self}
    fn validate_current(&self,item:&ConfirmationItem)->Result<()> {
        if item.status==ConfirmationStatus::Failed { return Err(Error::Denied); }
        let spec=allocation::specification(item).map_err(|_|Error::Invalid)?;
        if item.fields.iter().any(|f| !matches!(f.name.as_str(),"employee_id"|"disk_bytes"|"settings"|"binding_kind"|"owner_id"|"tenant_id"|"adopt_quarantine"))
            || !duduclaw_core::is_valid_agent_id(&spec.label) || duduclaw_core::is_reserved_agent_id(&spec.label) {return Err(Error::Denied);}
        // F6g: an adoption proposal names its quarantine row (digest-covered).
        let adopting=crate::isolation_adopt::proposal_ref(item).map_err(|_|Error::Invalid)?;
        // F6h R7: approval and apply refuse a label held by a quarantine row
        // (LABEL_QUARANTINED), except the row this adoption proposal names.
        crate::isolation_label_stores::check_label_not_quarantined(&self.home,&spec.label,
            adopting.as_ref().map(|adopt|(adopt.employee,adopt.incarnation))).map_err(label_hold_error)?;
        // Personal owner eligibility / Shared tenant pin, against users.db and apply.db.
        lifecycle::validate_proposed_binding(&self.home,&spec.label,spec.binding.as_ref()).map_err(lifecycle_error)?;
        if let Some(adopt)=&adopting {
            // The tree comes from the sealed row (or, once adopted, its marker), never the registry.
            let approver=self.approver(item)?;
            return self.with_adopt_env(&approver,|env| crate::isolation_adopt::validate_item(&self.home,env,item,adopt,&approver))
                .map(|_|()).map_err(adopt_error);
        }
        let expected:serde_json::Value=serde_json::from_str(text(item,"settings")?).map_err(|_|Error::Invalid)?;
        bounded(&self.live.handle,async {
            let registry=self.live.registry.read().await;
            let employee=registry.get(&spec.label).ok_or(Error::Denied)?;
            let current=serde_json::to_value(&employee.config).map_err(|_|Error::Invalid)?;
            // Require a complete fresh product configuration, not a partial
            // imported authority subset. Editing it invalidates this approval.
            if current!=expected{return Err(Error::Stale)}
            let fresh=crate::agent_files::flow::require_live_agent_access(&self.home,&self.live.context,&spec.label,duduclaw_auth::AccessLevel::Owner).map_err(|_|Error::Denied)?;
            if !fresh.is_admin(){return Err(Error::Denied)}
            Ok(())
        })?
    }
    fn exchange(&self,item:&ConfirmationItem,action:Action)->Result<Receipt>{exchange(item,action)}
    fn save(&self,item:&ConfirmationItem,receipt:&Receipt)->Result<()> {save_confirmed_receipt(&self.home,item,receipt)}
    /// F6g: the live Admin while the card is Pending, else the actor of the
    /// approval record (same rule as the retire card).
    fn approver(&self,item:&ConfirmationItem)->Result<String> {
        if item.status==ConfirmationStatus::Pending { return Ok(self.live.context.user_id.clone()); }
        let store=duduclaw_migration::confirmation::ConfirmationStore::from_read_only_connection(
            crate::fs_safe::sqlite::open_existing(&self.home,"migration-confirmations.db",crate::fs_safe::sqlite::Access::ReadOnly)
                .map_err(|_|Error::Storage)?).map_err(|_|Error::Storage)?;
        let actor=store.approver(&item.binding).map_err(|_|Error::Storage)?.ok_or(Error::Denied)?;
        Ok(uuid::Uuid::from_bytes(actor.0).to_string())
    }
    /// F6g collaborators: schema-2 root, catalog routes, run stopper, the
    /// F6c2 rule, and `agent_hook_installer` on the live runtime.
    fn with_adopt_env<T>(&self,actor:&str,run:impl FnOnce(&crate::isolation_adopt::AdoptEnv<'_>)->T)->T {
        let routes=crate::isolation_restore::CatalogRoutes(&self.home);
        let handle=self.live.handle.clone();
        let hooks=move |dir:&Path|->std::result::Result<(),()> {
            let bin=crate::agent_hook_installer::resolve_duduclaw_bin();
            bounded(&handle,crate::agent_hook_installer::ensure_agent_hook_settings(dir,&bin)).map_err(|_|())?.map(|_|()).map_err(|_|())
        };
        let env=crate::isolation_adopt::AdoptEnv { root:&crate::isolation_retire::SpawndRetireRoot, routes:&routes,
            runs:&crate::isolation_quarantine::SpawndRunStopper, rule:&duduclaw_auth::admin_approval_rule::EligibilityLogRule,
            hooks:&hooks, agent_cap:self.agent_cap, actor, step_up_verified_at:self.step_up_verified_at,
            now:chrono::Utc::now().timestamp() };
        run(&env)
    }
}
fn adopt_error(e:crate::isolation_adopt::AdoptError)->Error {
    use crate::isolation_adopt::AdoptError as A;
    match e {
        A::Unavailable | A::Restore(crate::isolation_restore::Refusal::Unavailable) => Error::Storage,
        A::SettingsChanged | A::DigestMismatch => Error::Stale,
        _ => Error::Denied,
    }
}

/// Apply bookkeeping for one root receipt. A Personal allocation also writes
/// the users.db mirror: users.db and apply.db cannot share an atomic
/// transaction (users.db is WAL), so the mirror is committed while the
/// apply.db writer lock is held and before the confirmed row; a mirror
/// failure refuses the step and leaves no confirmed row. Compensation
/// removes the mirror only after the disabled row is durable.
pub(crate) fn save_confirmed_receipt(home:&Path,item:&ConfirmationItem,receipt:&Receipt)->Result<()> {
    save_confirmed_receipt_gated(home,item,receipt,None)
}
/// [`save_confirmed_receipt`]; security round 3 N1: `gate` runs inside the
/// users.db write transaction that writes the Personal mirror, before it (or,
/// with no mirror to write, in its own users.db IMMEDIATE transaction), while
/// the apply.db writer lock is held (M6). A refusal writes neither the mirror
/// nor the confirmed row.
pub(crate) fn save_confirmed_receipt_gated(home:&Path,item:&ConfirmationItem,receipt:&Receipt,
    gate:Option<&lifecycle::UsersGate<'_>>)->Result<()> {
        let spec=allocation::specification(item).map_err(|_|Error::Invalid)?;
        // The root copies the principal from this confirmed proposal only.
        if receipt.binding!=item.binding || receipt.label!=spec.label || receipt.principal_binding!=spec.binding {return Err(Error::Stale);}
        // SR2 finding 7: the hardened private-SQLite opener (never created).
        let mut db=crate::fs_safe::sqlite::open_existing(&home.join("migration-authority"),"apply.db",
            crate::fs_safe::sqlite::Access::ReadWrite).map_err(|_|Error::Storage)?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_|Error::Storage)?;
        ensure_employee_schema(&db)?;
        let tenant = current_tenant(home, &receipt.label)?;
        let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_|Error::Storage)?;
        let previous:Option<(String,Option<String>)>=tx.query_row(
            "SELECT receipt,tenant_id FROM confirmed_employees WHERE label=?1",[&receipt.label],
            |r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|Error::Storage)?;
        let pinned_tenant = checked_tenant(previous.as_ref(), receipt, &tenant)?;
        let personal_owner=match &receipt.principal_binding {
            // Shared carries the proposal tenant; it must be the pinned one.
            Some(EmployeeBinding::Shared(t)) if !receipt.disabled && pinned_tenant.as_deref()!=Some(t.as_str()) => return Err(Error::Denied),
            Some(EmployeeBinding::Personal(owner)) => Some(owner.as_str()),
            _ => None,
        };
        if let (Some(owner),false)=(personal_owner,receipt.disabled) {
            lifecycle::record_personal_mirror_gated(home,&receipt.label,owner,gate).map_err(lifecycle_error)?;
        } else if let Some(gate)=gate {
            lifecycle::run_users_gate(home,gate).map_err(lifecycle_error)?;
        }
        // F6 §3.1: only restore writes `incarnation` (+1). A re-save of the
        // same employee id preserves it; a new id on the label starts at 0
        // (adoption and label reuse never inherit an incarnation).
        let same_employee=match &previous {
            Some((encoded,_))=>serde_json::from_str::<Receipt>(encoded).map_err(|_|Error::Storage)?.employee==receipt.employee,
            None=>false,
        };
        tx.execute("INSERT INTO confirmed_employees(label,operation,digest,receipt,tenant_id,incarnation) VALUES(?1,?2,?3,?4,?5,0)
            ON CONFLICT(label) DO UPDATE SET operation=excluded.operation,digest=excluded.digest,receipt=excluded.receipt,tenant_id=excluded.tenant_id,
            incarnation=CASE WHEN ?6 THEN confirmed_employees.incarnation ELSE 0 END",
            params![receipt.label,operation(&item.binding),item.binding.proposal_digest.0.as_slice(),serde_json::to_string(receipt).map_err(|_|Error::Storage)?,pinned_tenant,same_employee]).map_err(|_|Error::Storage)?;
        tx.commit().map_err(|_|Error::Storage)?;
        if let (Some(owner),true)=(personal_owner,receipt.disabled) {
            // Another owner's mirror (Stale) is not this allocation's to remove.
            match lifecycle::remove_personal_mirror(home,&receipt.label,owner) {
                Ok(_)|Err(LifecycleError::Stale)=>{}
                Err(e)=>return Err(lifecycle_error(e)),
            }
        }
        Ok(())
}
fn label_hold_error(e:crate::isolation_label_stores::GateRefusal)->Error {
    match e { crate::isolation_label_stores::GateRefusal::Unavailable=>Error::Storage, _=>Error::Denied }
}
fn lifecycle_error(e:LifecycleError)->Error {
    match e {
        LifecycleError::Unavailable=>Error::Storage,
        LifecycleError::Denied|LifecycleError::Tombstoned=>Error::Denied,
        LifecycleError::Stale=>Error::Stale,
    }
}
fn exchange(item:&ConfirmationItem,action:Action)->Result<Receipt>{
    #[cfg(target_os="linux")]{
        allocation::transport::request(&Request{schema:1,nonce:RequestId(*uuid::Uuid::new_v4().as_bytes()),action,item:item.clone(),approver:None,
            entitlement:None}).map_err(|_|Error::Apply)
    }
    #[cfg(not(target_os="linux"))]{let _=(item,action);Err(Error::Denied)}
}
impl MigrationAuthorityBackend for EmployeeAuthority {
    fn supports(&self,category:ConfirmationCategory)->bool{cfg!(target_os="linux") && category==ConfirmationCategory::Employee}
    fn validate(&self,item:&ConfirmationItem)->Result<()>{if !self.supports(item.category){return Err(Error::Denied)}self.validate_current(item)}
    fn stage(&self,item:&ConfirmationItem)->Result<()>{
        self.validate(item)?;
        // R7d: `Err(Pending)` leaves the step resumable (GatewayAuthority
        // re-runs this stage; the root resumes the same reservation).
        // R8d: the desired entitlement before any account, Entitle after the
        // Ready receipt and before anything is saved.
        let receipt=allocate_entitled(self.root.as_ref(),self.planner.as_ref(),super::shutdown_gate(),item)?;
        if crate::isolation_adopt::proposal_ref(item).map_err(|_|Error::Invalid)?.is_some() {
            // F6g §3.10 steps 4–5: transform, re-key, marker, confirmed row, commit.
            let approver=self.approver(item)?;
            // Security round 3 N1: the commit's owner/approval re-check runs in
            // the users.db write transaction of the Personal mirror.
            let mut save=|gate:&dyn Fn(&Connection)->std::result::Result<(),()>|
                save_confirmed_receipt_gated(&self.home,item,&receipt,Some(gate)).map_err(|_|());
            return self.with_adopt_env(&approver,|env| crate::isolation_adopt::apply_item(&self.home,env,item,&receipt,&approver,&mut save))
                .map(|_|()).map_err(adopt_error);
        }
        self.save(item,&receipt)
    }
    fn publish(&self,item:&ConfirmationItem)->Result<()>{self.validate(item)?;let receipt=self.exchange(item,Action::ReadBack)?;self.save(item,&receipt)}
    fn read_back(&self,item:&ConfirmationItem)->Result<()>{self.publish(item)}
    fn disable(&self,item:&ConfirmationItem)->Result<()>{
        let receipt=self.exchange(item,Action::Disable)?;self.save(item,&receipt)?;
        // F6g §3.10 step 6: the moved entries go back, the row needs a reseal.
        if crate::isolation_adopt::proposal_ref(item).map_err(|_|Error::Compensation)?.is_some() {
            let actor=self.live.context.user_id.clone();
            self.with_adopt_env(&actor,|env| crate::isolation_adopt::compensate_item(&self.home,env,item)).map_err(|_|Error::Compensation)?;
        }
        Ok(())
    }
    fn readback_shared_employee(&self,agent:&str)->Result<()>{
        let (_,employee)=lifecycle::resolve(&self.home,agent,&lifecycle::RootAllocator).map_err(lifecycle_error)?;
        // Shared ACL/grant restoration never targets a Personal employee.
        if !matches!(employee.binding,EmployeeBinding::Shared(_)){return Err(Error::Denied)}
        Ok(())
    }
}

/// Read-only label resolution for A/D, now the `EmployeeLifecycle` read model
/// (`crate::isolation_lifecycle`). A stored mapping is only an index: each
/// lookup rechecks the Applied confirmation (including its principal), the
/// root UID/quota/principal readback, the tenant pin and, for a Personal
/// employee, the users.db mirror owner. A deleted or replaced users database
/// cannot transfer an employee to a new tenant.
pub(crate) fn resolve_confirmed_employee(home:&Path,label:&str)->Result<Receipt>{
    lifecycle::resolve(home,label,&lifecycle::RootAllocator).map(|(receipt,_)|receipt).map_err(lifecycle_error)
}

// Extend the existing allocation index rather than creating a second ACL store.
// F6a: the schema (incl. `incarnation`, the quarantine and root-allocation
// tables) lives in `isolation_quarantine`; a newer schema refuses.
fn ensure_employee_schema(db:&Connection)->Result<()> {
    crate::isolation_quarantine::ensure_employee_schema(db).map_err(|_|Error::Storage)
}
fn current_tenant(home:&Path,agent:&str)->Result<String> {
    // SR2 finding 7: the hardened private-SQLite opener, read-only.
    let mut users=crate::fs_safe::sqlite::open_existing(home,"users.db",crate::fs_safe::sqlite::Access::ReadOnly).map_err(|_|Error::Storage)?;
    users.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_|Error::Storage)?;
    duduclaw_auth::read_isolation_authority(&mut users,None,agent)
        .map(|snapshot|snapshot.tenant_id).map_err(|_|Error::Denied)
}
fn checked_tenant(previous:Option<&(String,Option<String>)>,receipt:&Receipt,tenant:&str)->Result<Option<String>> {
    if let Some((encoded,pinned))=previous {
        let prior:Receipt=serde_json::from_str(encoded).map_err(|_|Error::Storage)?;
        if prior.employee==receipt.employee {
            // The principal is immutable for an employee's whole lifetime.
            if prior.principal_binding!=receipt.principal_binding {return Err(Error::Stale);}
            // Compensation may disable an old unpinned row, but never assigns
            // that employee a tenant retroactively or transfers its lifetime.
            if receipt.disabled { return Ok(pinned.clone()); }
            if prior.disabled {return Err(Error::Denied);}
            if pinned.as_deref()!=Some(tenant) {return Err(Error::Denied);}
        } else if !prior.disabled {return Err(Error::Stale);}
        if prior.binding!=receipt.binding && !prior.disabled {return Err(Error::Stale);}
    }
    Ok(Some(tenant.to_owned()))
}

#[cfg(test)]
mod isolation_tenant_tests {
    use super::*;
    fn receipt(employee:u8,disabled:bool)->Receipt {
        Receipt { binding:ProposalBinding { operation_id:ProposalOperationId {
            proposal_id:duduclaw_migration::ids::RequestId([1;16]),revision:1 },
            proposal_digest:Sha256Digest([2;32]) }, label:"sales".into(),
            employee:duduclaw_isolation::ids::EmployeeId([employee;16]),uid:70001,
            bytes:67108864,inodes:4096,disabled,principal_binding:None }
    }
    #[test]
    fn isolation_tenant_pin_refuses_transfer_backfill_and_resurrection() {
        let active=receipt(1,false);
        let pinned=(serde_json::to_string(&active).unwrap(),Some("tenant-a".into()));
        assert_eq!(checked_tenant(Some(&pinned),&active,"tenant-a").unwrap(),Some("tenant-a".into()));
        assert!(checked_tenant(Some(&pinned),&active,"tenant-b").is_err());
        let missing=(pinned.0.clone(),None);
        assert!(checked_tenant(Some(&missing),&active,"tenant-a").is_err());
        let disabled=receipt(1,true);
        assert_eq!(checked_tenant(Some(&missing),&disabled,"tenant-a").unwrap(),None);
        let dead=(serde_json::to_string(&disabled).unwrap(),Some("tenant-a".into()));
        assert!(checked_tenant(Some(&dead),&active,"tenant-a").is_err());
        assert_eq!(checked_tenant(Some(&dead),&receipt(2,false),"tenant-b").unwrap(),Some("tenant-b".into()));
        assert!(checked_tenant(Some(&pinned),&receipt(2,false),"tenant-a").is_err());
    }
    #[test]
    fn isolation_tenant_schema_preserves_unpinned_legacy_row() {
        let db=Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE confirmed_employees(label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,digest BLOB NOT NULL,receipt TEXT NOT NULL);
            INSERT INTO confirmed_employees VALUES('sales','op',X'01','legacy');").unwrap();
        ensure_employee_schema(&db).unwrap();
        ensure_employee_schema(&db).unwrap();
        let pinned:Option<String>=db.query_row("SELECT tenant_id FROM confirmed_employees WHERE label='sales'",[],|r|r.get(0)).unwrap();
        assert_eq!(pinned,None);
    }
}
