//! Confirmed allocation for a freshly configured employee. Imported settings
//! alone never create an enabled product employee or bypass normal product writes.
use super::*;
use duduclaw_migration::allocation::{self, Action, Receipt, Request};
use duduclaw_migration::ids::RequestId;
use std::sync::Arc;

pub(crate) struct EmployeeAuthority { home:PathBuf, live:Arc<LiveAuthority> }
impl EmployeeAuthority {
    pub(crate) fn new(home:PathBuf,live:Arc<LiveAuthority>)->Self{Self{home,live}}
    fn validate_current(&self,item:&ConfirmationItem)->Result<()> {
        if item.status==ConfirmationStatus::Failed { return Err(Error::Denied); }
        let spec=allocation::specification(item).map_err(|_|Error::Invalid)?;
        if item.fields.iter().any(|f| !matches!(f.name.as_str(),"employee_id"|"disk_bytes"|"settings"))
            || !duduclaw_core::is_valid_agent_id(&spec.label) || duduclaw_core::is_reserved_agent_id(&spec.label) {return Err(Error::Denied);}
        let expected:serde_json::Value=serde_json::from_str(text(item,"settings")?).map_err(|_|Error::Invalid)?;
        self.live.handle.block_on(async {
            let registry=self.live.registry.read().await;
            let employee=registry.get(&spec.label).ok_or(Error::Denied)?;
            let current=serde_json::to_value(&employee.config).map_err(|_|Error::Invalid)?;
            // Require a complete fresh product configuration, not a partial
            // imported authority subset. Editing it invalidates this approval.
            if current!=expected{return Err(Error::Stale)}
            let fresh=crate::agent_files::flow::require_live_agent_access(&self.home,&self.live.context,&spec.label,duduclaw_auth::AccessLevel::Owner).map_err(|_|Error::Denied)?;
            if !fresh.is_admin(){return Err(Error::Denied)}
            Ok(())
        })
    }
    fn exchange(&self,item:&ConfirmationItem,action:Action)->Result<Receipt>{exchange(item,action)}
    fn save(&self,item:&ConfirmationItem,receipt:&Receipt)->Result<()> {
        let path=self.home.join("migration-authority/apply.db");
        let mut db=Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_|Error::Storage)?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_|Error::Storage)?;
        ensure_employee_schema(&db)?;
        let tenant = current_tenant(&self.home, &receipt.label)?;
        let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_|Error::Storage)?;
        let previous:Option<(String,Option<String>)>=tx.query_row(
            "SELECT receipt,tenant_id FROM confirmed_employees WHERE label=?1",[&receipt.label],
            |r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|Error::Storage)?;
        let pinned_tenant = checked_tenant(previous.as_ref(), receipt, &tenant)?;
        tx.execute("INSERT INTO confirmed_employees(label,operation,digest,receipt,tenant_id) VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(label) DO UPDATE SET operation=excluded.operation,digest=excluded.digest,receipt=excluded.receipt,tenant_id=excluded.tenant_id",
            params![receipt.label,operation(&item.binding),item.binding.proposal_digest.0.as_slice(),serde_json::to_string(receipt).map_err(|_|Error::Storage)?,pinned_tenant]).map_err(|_|Error::Storage)?;
        tx.commit().map_err(|_|Error::Storage)

    }
}
fn exchange(item:&ConfirmationItem,action:Action)->Result<Receipt>{
    #[cfg(target_os="linux")]{
        allocation::transport::request(&Request{schema:1,nonce:RequestId(*uuid::Uuid::new_v4().as_bytes()),action,item:item.clone()}).map_err(|_|Error::Apply)
    }
    #[cfg(not(target_os="linux"))]{let _=(item,action);Err(Error::Denied)}
}
impl MigrationAuthorityBackend for EmployeeAuthority {
    fn supports(&self,category:ConfirmationCategory)->bool{cfg!(target_os="linux") && category==ConfirmationCategory::Employee}
    fn validate(&self,item:&ConfirmationItem)->Result<()>{if !self.supports(item.category){return Err(Error::Denied)}self.validate_current(item)}
    fn stage(&self,item:&ConfirmationItem)->Result<()>{
        self.validate(item)?;
        let receipt=self.exchange(item,Action::Allocate)?;self.save(item,&receipt)
    }
    fn publish(&self,item:&ConfirmationItem)->Result<()>{self.validate(item)?;let receipt=self.exchange(item,Action::ReadBack)?;self.save(item,&receipt)}
    fn read_back(&self,item:&ConfirmationItem)->Result<()>{self.publish(item)}
    fn disable(&self,item:&ConfirmationItem)->Result<()>{let receipt=self.exchange(item,Action::Disable)?;self.save(item,&receipt)}
    fn readback_shared_employee(&self,agent:&str)->Result<()>{resolve_confirmed_employee(&self.home,agent).map(|_|())}
}

/// Read-only label resolution for A/D. A stored mapping is only an index: each
/// lookup rechecks the Applied confirmation and the root UID/quota readback.
/// The tenant pin lives in this existing confirmed registry; a deleted or
/// replaced users database cannot transfer an employee to a new tenant.
pub(crate) fn resolve_confirmed_employee(home:&Path,label:&str)->Result<Receipt>{
    let db=Connection::open_with_flags(home.join("migration-authority/apply.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_|Error::Storage)?;
    let (encoded,tenant):(String,Option<String>)=db.query_row("SELECT receipt,tenant_id FROM confirmed_employees WHERE label=?1",[label],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|Error::Denied)?;
    if tenant.as_deref()!=Some(current_tenant(home,label)?.as_str()){return Err(Error::Denied);}
    let prior:Receipt=serde_json::from_str(&encoded).map_err(|_|Error::Storage)?;
    if prior.disabled || prior.label!=label{return Err(Error::Denied)}
    let store=duduclaw_migration::confirmation::ConfirmationStore::open_read_only(&home.join("migration-confirmations.db")).map_err(|_|Error::Storage)?;
    let item=store.list().map_err(|_|Error::Storage)?.into_iter().find(|i|i.binding==prior.binding && i.status==ConfirmationStatus::Applied).ok_or(Error::Denied)?;
    let current=exchange(&item,Action::ReadBack)?;
    if current.disabled || current.employee!=prior.employee || current.uid!=prior.uid
        || current.binding!=prior.binding || current.label!=prior.label
        || current.bytes!=prior.bytes || current.inodes!=prior.inodes {return Err(Error::Stale)}Ok(current)
}

// Extend the existing allocation index rather than creating a second ACL store.
fn ensure_employee_schema(db:&Connection)->Result<()> {
    db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS confirmed_employees (
        label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,digest BLOB NOT NULL,
        receipt TEXT NOT NULL,tenant_id TEXT);").map_err(|_|Error::Storage)?;
    let columns=db.prepare("PRAGMA table_info(confirmed_employees)").map_err(|_|Error::Storage)?
        .query_map([],|r|r.get::<_,String>(1)).map_err(|_|Error::Storage)?
        .collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::Storage)?;
    if !columns.iter().any(|column|column=="tenant_id") {
        db.execute_batch("ALTER TABLE confirmed_employees ADD COLUMN tenant_id TEXT;").map_err(|_|Error::Storage)?;
    }
    Ok(())
}
fn current_tenant(home:&Path,agent:&str)->Result<String> {
    let mut users=Connection::open_with_flags(home.join("users.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_|Error::Storage)?;
    duduclaw_auth::read_isolation_authority(&mut users,None,agent)
        .map(|snapshot|snapshot.tenant_id).map_err(|_|Error::Denied)
}
fn checked_tenant(previous:Option<&(String,Option<String>)>,receipt:&Receipt,tenant:&str)->Result<Option<String>> {
    if let Some((encoded,pinned))=previous {
        let prior:Receipt=serde_json::from_str(encoded).map_err(|_|Error::Storage)?;
        if prior.employee==receipt.employee {
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
            bytes:67108864,inodes:4096,disabled }
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
