//! Confirmed allocation for a freshly configured employee. Imported settings
//! alone never create an enabled product employee or bypass normal product writes.
use super::*;
use duduclaw_migration::allocation::{self, Action, Receipt, Request};
use duduclaw_migration::ids::RequestId;
use duduclaw_isolation::capability::EmployeeBinding;
use crate::isolation_lifecycle::{self as lifecycle, LifecycleError};
use std::sync::Arc;

pub(crate) struct EmployeeAuthority { home:PathBuf, live:Arc<LiveAuthority> }
impl EmployeeAuthority {
    pub(crate) fn new(home:PathBuf,live:Arc<LiveAuthority>)->Self{Self{home,live}}
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
            hooks:&hooks, agent_cap:0, actor, now:chrono::Utc::now().timestamp() };
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
            lifecycle::record_personal_mirror(home,&receipt.label,owner).map_err(lifecycle_error)?;
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
        allocation::transport::request(&Request{schema:1,nonce:RequestId(*uuid::Uuid::new_v4().as_bytes()),action,item:item.clone(),approver:None}).map_err(|_|Error::Apply)
    }
    #[cfg(not(target_os="linux"))]{let _=(item,action);Err(Error::Denied)}
}
impl MigrationAuthorityBackend for EmployeeAuthority {
    fn supports(&self,category:ConfirmationCategory)->bool{cfg!(target_os="linux") && category==ConfirmationCategory::Employee}
    fn validate(&self,item:&ConfirmationItem)->Result<()>{if !self.supports(item.category){return Err(Error::Denied)}self.validate_current(item)}
    fn stage(&self,item:&ConfirmationItem)->Result<()>{
        self.validate(item)?;
        let receipt=self.exchange(item,Action::Allocate)?;
        if crate::isolation_adopt::proposal_ref(item).map_err(|_|Error::Invalid)?.is_some() {
            // F6g §3.10 steps 4–5: transform, re-key, marker, confirmed row, commit.
            let approver=self.approver(item)?;
            let mut save=||self.save(item,&receipt).map_err(|_|());
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
