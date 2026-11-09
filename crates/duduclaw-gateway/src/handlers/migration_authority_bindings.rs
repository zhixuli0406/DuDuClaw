//! Shared ACL restoration after separate employee approval and fresh user login.
use super::*;
#[derive(Serialize,Deserialize)]
pub(super) struct BindingPlan {
    operation:String,digest:[u8;32],user_id:String,email:String,agent:String,access:duduclaw_auth::AccessLevel,
}
fn confirmed_employee(home:&Path,agent:&str)->Result<()> {
    if !duduclaw_core::is_valid_agent_id(agent){return Err(Error::Invalid);}
    let store=duduclaw_migration::confirmation::ConfirmationStore::open_read_only(&home.join("migration-confirmations.db"))?;
    if !store.list()?.iter().any(|item|item.category==ConfirmationCategory::Employee && item.status==ConfirmationStatus::Applied
        && text(item,"employee_id").ok()==Some(agent)) {return Err(Error::Denied);}
    let path=home.join("agents").join(agent).join("agent.toml");validate_parents(home,&path)?;regular(&path)?;Ok(())
}
fn db(home:&Path)->Result<duduclaw_auth::UserDb>{regular(&home.join("users.db"))?;duduclaw_auth::UserDb::new(&home.join("users.db")).map_err(|_|Error::Storage)}
pub(super) fn binding_plan(home:&Path,item:&ConfirmationItem)->Result<BindingPlan>{
    let agent=text(item,"employee_id")?;confirmed_employee(home,agent)?;
    let access=text(item,"binding_kind")?.strip_prefix("shared:").ok_or(Error::Denied)?.parse().map_err(|_|Error::Invalid)?;
    let email=text(item,"owner_id")?;let user=db(home)?.get_user_by_email(email).map_err(|_|Error::Storage)?.ok_or(Error::Denied)?;
    if user.status!=duduclaw_auth::UserStatus::Active || user.must_change_password || user.last_login.is_none(){return Err(Error::Denied);}
    Ok(BindingPlan{operation:operation(&item.binding),digest:item.binding.proposal_digest.0,user_id:user.id,email:email.into(),agent:agent.into(),access})
}
impl BindingPlan {
    pub(super) fn check_absent(&self,home:&Path)->Result<()>{
        if db(home)?.get_user_agents(&self.user_id).map_err(|_|Error::Storage)?.iter().any(|b|b.agent_name==self.agent){return Err(Error::Stale);}Ok(())
    }
    pub(super) fn publish(&self,home:&Path)->Result<()>{
        confirmed_employee(home,&self.agent)?;
        db(home)?.migration_bind_shared(&self.operation,&self.digest,&self.user_id,&self.agent,self.access).map_err(|_|Error::Apply)
    }
    pub(super) fn read_back(&self,home:&Path)->Result<()>{
        confirmed_employee(home,&self.agent)?;
        let db=db(home)?;let user=db.get_user(&self.user_id).map_err(|_|Error::Storage)?.ok_or(Error::Apply)?;
        let binding=db.migration_binding_readback(&self.operation,&self.digest).map_err(|_|Error::Apply)?;
        if user.email!=self.email || binding.user_id!=self.user_id || binding.agent_name!=self.agent || binding.access_level!=self.access {return Err(Error::Apply);}Ok(())
    }
    pub(super) fn disable(&self,home:&Path)->Result<()>{
        db(home)?.migration_binding_disable(&self.operation,&self.digest).map_err(|_|Error::Compensation)
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test]
    fn migration_authority_shared_label_never_substitutes_for_principal_readback(){
        let home=tempfile::tempdir().unwrap();
        // Satisfy every pre-existing prerequisite; only the new immutable
        // Shared-principal readback remains unavailable in this counterfactual.
        struct EmployeeFixture;
        impl ProposalValidator for EmployeeFixture {fn validate_fresh(&self,_:&ConfirmationItem)->Result<()>{Ok(())}}
        impl ProposalApplier for EmployeeFixture {
            fn stage(&mut self,_:&ConfirmationItem)->Result<()>{Ok(())}
            fn publish(&mut self,_:&ConfirmationItem)->Result<()>{Ok(())}
            fn read_back(&mut self,_:&ConfirmationItem)->Result<()>{Ok(())}
            fn disable_and_read_back(&mut self,_:&ConfirmationItem)->Result<()>{Ok(())}
        }
        let path=home.path().join("migration-confirmations.db");
        let mut store=duduclaw_migration::confirmation::ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
        let employee=store.stage_proposal(ProposalOperationId{proposal_id:duduclaw_migration::ids::RequestId([1;16]),revision:1},
            ConfirmationCategory::Employee,vec![ProposalField{name:"employee_id".into(),value:ProposalValue::Text{value:"sales".into()}}],&EmployeeFixture).unwrap();
        let session=duduclaw_migration::confirmation::ApprovalSession::from_verified_session(true,true,Some(100),[2;32],duduclaw_migration::ids::RequestId([3;16]));
        store.approve(&[employee.binding],&session,[2;32],100,&EmployeeFixture).unwrap();
        for _ in 0..3{store.apply_next(&employee.binding,&mut EmployeeFixture).unwrap();}
        std::fs::create_dir_all(home.path().join("agents/sales")).unwrap();
        std::fs::write(home.path().join("agents/sales/agent.toml"),"name='sales'\n").unwrap();
        let users=duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user=users.migration_invite_user("abc-1",&[1;32],"new@example.test","New",duduclaw_auth::UserRole::Employee).unwrap();
        Connection::open(home.path().join("users.db")).unwrap().execute("UPDATE users SET must_change_password=0,last_login='now' WHERE id=?1",[&user.id]).unwrap();
        let authority=GatewayAuthority::new(home.path().into()).unwrap();
        let item=ConfirmationItem{binding:ProposalBinding{operation_id:ProposalOperationId{
            proposal_id:duduclaw_migration::ids::RequestId([7;16]),revision:1},proposal_digest:Sha256Digest([8;32])},
            category:ConfirmationCategory::UserAgentBinding,status:ConfirmationStatus::Pending,high_risk:true,
            fields:[("employee_id","sales"),("owner_id","new@example.test"),("binding_kind","shared:owner")].into_iter()
                .map(|(name,value)|ProposalField{name:name.into(),value:ProposalValue::Text{value:value.into()}}).collect()};
        assert!(binding_plan(home.path(),&item).is_ok());
        assert!(matches!(authority.validate_fresh(&item),Err(Error::Denied)));
        assert!(users.get_user_agents(&user.id).unwrap().is_empty());
    }
}
