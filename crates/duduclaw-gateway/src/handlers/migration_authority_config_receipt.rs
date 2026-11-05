//! Ownership is recorded from the normal writer's fsynced temporary file before
//! rename. Bytes alone cannot distinguish migration from an ordinary equal edit.
use super::*;

#[derive(Serialize,Deserialize,PartialEq,Eq)]
struct Identity {device:u64,inode:u64,mtime:i64,nanos:i64,len:u64}
fn identity(meta:&std::fs::Metadata)->Result<Identity> {
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if !meta.is_file() {return Err(Error::Storage);}
        Ok(Identity{device:meta.dev(),inode:meta.ino(),mtime:meta.mtime(),nanos:meta.mtime_nsec(),len:meta.len()})
    }
    #[cfg(not(unix))] {let _=meta;Err(Error::Denied)}
}
impl GatewayAuthority {
    fn record_config_file(&self,item:&ConfirmationItem,phase:&str,file:&std::fs::File)->Result<()> {
        let proof=serde_json::to_vec(&identity(&file.metadata().map_err(|_|Error::Storage)?)?).map_err(|_|Error::Storage)?;
        let changed=self.db.execute("INSERT INTO config_publications(operation,digest,phase,identity) VALUES(?1,?2,?3,?4)
            ON CONFLICT(operation,phase) DO UPDATE SET identity=excluded.identity WHERE digest=excluded.digest",
            params![operation(&item.binding),item.binding.proposal_digest.0.as_slice(),phase,proof]).map_err(|_|Error::Storage)?;
        if changed!=1 {return Err(Error::Stale);}Ok(())
    }
    fn owns_config(&self,item:&ConfirmationItem,path:&Path)->Result<bool> {
        let row:Option<(Vec<u8>,Vec<u8>)>=self.db.query_row("SELECT digest,identity FROM config_publications WHERE operation=?1 AND phase='publish'",
            [operation(&item.binding)],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|Error::Storage)?;
        let Some((digest,proof))=row else {return Ok(false);};
        if digest!=item.binding.proposal_digest.0 {return Err(Error::Stale);}
        let expected:Identity=serde_json::from_slice(&proof).map_err(|_|Error::Storage)?;
        let metadata=std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?;
        if !metadata.file_type().is_file() {return Err(Error::Storage);}
        Ok(identity(&metadata)?==expected)
    }
    pub(super) fn publish_config(&self,item:&ConfirmationItem,relative:&str,before:&str,after:&str)->Result<()> {
        let path=self.home.join(relative);let current=read_optional(&path)?;
        if before==after {return if current==after {Ok(())}else{Err(Error::Stale)};}
        if current==after {return if self.owns_config(item,&path)? {Ok(())}else{Err(Error::Stale)};}
        config_commit::commit_text_observed(&path,&config_commit::content_hash(before),after,
            &mut |file|self.record_config_file(item,"publish",file).map_err(|_|"migration config receipt refused".into())).map_err(|_|Error::Apply)
    }
    pub(super) fn read_back_config(&self,item:&ConfirmationItem,relative:&str,before:&str,after:&str)->Result<()> {
        if before!=after && !self.owns_config(item,&self.home.join(relative))? {return Err(Error::Stale);}
        self.verify_effective_config(item,relative,after)
    }
    pub(super) fn disable_config(&self,item:&ConfirmationItem,relative:&str,before:&str,after:&str)->Result<()> {
        let path=self.home.join(relative);let current=read_optional(&path)?;
        if current==before {return self.verify_effective_config(item,relative,before);}
        if current!=after || !self.owns_config(item,&path)? {return Err(Error::Compensation);}
        // Check ownership again under the normal writer's lock. Another normal
        // save of identical bytes must not pass merely because its hash matches.
        config_commit::commit_text_observed_guarded(&path,&config_commit::content_hash(after),before,
            ||if self.owns_config(item,&path).unwrap_or(false) {Ok(())}else{Err("migration config ownership changed".into())},
            &mut |file|self.record_config_file(item,"rollback",file).map_err(|_|"migration config receipt refused".into())).map_err(|_|Error::Compensation)?;
        self.verify_effective_config(item,relative,before).map_err(|_|Error::Compensation)
    }
}

#[cfg(all(test,unix))]
mod tests {
    use super::*;
    fn proposal()->ConfirmationItem {
        ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{proposal_id:duduclaw_migration::ids::RequestId([5;16]),revision:1},proposal_digest:Sha256Digest([6;32])},
            category:ConfirmationCategory::Policy,status:ConfirmationStatus::Applying,high_risk:true,
            fields:vec![ProposalField{name:"name".into(),value:ProposalValue::Text{value:"provenance".into()}},
                ProposalField{name:"rules".into(),value:ProposalValue::Text{value:"policy='enforce'".into()}}]}
    }
    #[test]
    fn migration_authority_config_refuses_equal_normal_publication_and_compensation() {
        for after_publish in [false,true] {
            let home=tempfile::tempdir().unwrap();let path=home.path().join("config.toml");std::fs::write(&path,"").unwrap();
            let item=proposal();let mut authority=GatewayAuthority::new(home.path().into()).unwrap();authority.stage(&item).unwrap();
            let Some(Plan::Config{after,..})=authority.plan(&item).unwrap() else {panic!("config plan");};
            if after_publish {authority.publish(&item).unwrap();authority.read_back(&item).unwrap();}
            // A normal replacement with equal bytes gets another inode. Even
            // after a crash-like retry, it must never be owned by this proposal.
            let current=read_optional(&path).unwrap();
            let same_projection=format!("{after}\n");
            config_commit::commit_text_locked(&path,&config_commit::content_hash(&current),&same_projection).unwrap();
            config_commit::commit_text_locked(&path,&config_commit::content_hash(&same_projection),&after).unwrap();
            assert!(authority.publish(&item).is_err());assert!(authority.read_back(&item).is_err());assert!(authority.disable_and_read_back(&item).is_err());
            assert_eq!(read_optional(&path).unwrap(),after);
        }
    }
    #[test]
    fn migration_authority_config_receipt_survives_restart_and_rejects_google_source_change() {
        let home=tempfile::tempdir().unwrap();let path=home.path().join("config.toml");std::fs::write(&path,"").unwrap();let item=proposal();
        let mut authority=GatewayAuthority::new(home.path().into()).unwrap();authority.stage(&item).unwrap();authority.publish(&item).unwrap();drop(authority);
        let mut authority=GatewayAuthority::new(home.path().into()).unwrap();authority.publish(&item).unwrap();authority.read_back(&item).unwrap();
        authority.disable_and_read_back(&item).unwrap();authority.publish(&item).unwrap();authority.read_back(&item).unwrap();
        let before=read_optional(&path).unwrap();let next=format!("{before}\n[integrations]\ngoogle_credential_path='/new/path'\n");
        let mut observed=false;
        assert!(config_commit::commit_text_observed(&path,&config_commit::content_hash(&before),&next,&mut |_|{observed=true;Ok(())}).is_err());
        assert!(!observed);assert_eq!(read_optional(&path).unwrap(),before);
    }
}
