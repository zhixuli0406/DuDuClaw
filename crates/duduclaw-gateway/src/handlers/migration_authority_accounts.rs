//! Narrow live readback for migrated API-key accounts. No provider request.
//! Only inline encrypted account credentials and disabled host inheritance are
//! accepted, so rebuilding the ordinary rotator cannot discover host credentials
//! or resolve a network secret backend.
use super::*;
use duduclaw_security::secret_ref::{Secret, SecretRef};

pub(crate) struct GatewayAccountReadback {
    home: PathBuf,
    handle: tokio::runtime::Handle,
}
impl GatewayAccountReadback {
    pub(crate) fn new(home: PathBuf, handle: tokio::runtime::Handle) -> Self { Self {home,handle} }
    pub(crate) fn item_supported(item: &ConfirmationItem) -> bool { account_input(item).is_ok() }
    pub(crate) fn available(home: &Path, item: &ConfirmationItem) -> bool {
        Self::item_supported(item) && read_optional(&home.join("config.toml")).ok()
            .and_then(|s| s.parse::<toml::Table>().ok()).is_some_and(|t| safe_loader_inputs(&t).is_ok())
    }
}
impl MigrationConfigReadback for GatewayAccountReadback {
    fn supports(&self, item: &ConfirmationItem) -> bool { Self::available(&self.home,item) }
    fn reload_and_verify(&self, item: &ConfirmationItem, relative: &str, expected: &str) -> Result<()> {
        if relative != "config.toml" {return Err(Error::Denied);}
        let input=account_input(item)?;
        let table:toml::Table=expected.parse().map_err(|_|Error::Invalid)?;
        safe_loader_inputs(&table)?;
        let target=find_account(&table,&input.id)?;
        let target=match target {
            Some(account)=>Some((read_account_secret(&account,&self.home)?,account)),None=>None,
        };
        let home=self.home.clone();
        let expected=expected.to_owned();
        // The caller runs on spawn_blocking. Use the gateway's existing runtime,
        // not a second runtime and not a detached task that can outlive the RPC.
        self.handle.block_on(async move {
            tokio::time::timeout(std::time::Duration::from_secs(10),async {
                if read_optional(&home.join(relative))?!=expected {return Err(Error::Stale);}
                crate::claude_runner::invalidate_rotator_cache().await;
                let rotator=crate::claude_runner::get_rotator_cached(&home).await.map_err(|_|Error::Apply)?;
                let statuses=rotator.status().await;
                let matches:Vec<_>=statuses.iter().filter(|s|s.id==input.id).collect();
                let Some((secret,account))=target else {
                    return if matches.is_empty() {Ok(())} else {Err(Error::Compensation)};
                };
                if matches.len()!=1 {return Err(Error::Apply);}
                let state=matches[0];
                let provider=string(&account,"provider")?;
                if provider!=input.provider || state.provider!=provider || state.auth_method!="apikey"
                    || state.priority as i64!=integer(&account,"priority",10)?
                    || state.monthly_budget_cents as i64!=integer(&account,"monthly_budget_cents",5000)? {
                    return Err(Error::Apply);
                }
                let selected=rotator.select_for_provider_with_pool(provider,&[input.id.clone()]).await.ok_or(Error::Apply)?;
                if selected.id!=input.id || selected.provider!=provider
                    || selected.raw_key.as_deref()!=Some(secret.expose()) {return Err(Error::Apply);}
                if read_optional(&home.join(relative))?!=expected {return Err(Error::Stale);}
                Ok(())
            }).await.map_err(|_|Error::Apply)?
        })
    }
}

pub(super) struct AccountInput { pub id:String, pub provider:String, pub priority:u32, pub budget:u64 }
pub(super) fn account_input(item:&ConfirmationItem)->Result<AccountInput> {
    if item.category!=ConfirmationCategory::ConfigurationSection || text(item,"section")?!="accounts" {return Err(Error::Denied);}
    let settings:toml::Table=text(item,"settings")?.parse().map_err(|_|Error::Invalid)?;
    if settings.keys().any(|k|!matches!(k.as_str(),"id"|"type"|"provider"|"api_key"|"priority"|"monthly_budget_cents"))
        || string(&settings,"type")?!="api_key" || string(&settings,"api_key")?!=config_commit::RAW_SECRET_MASK {return Err(Error::Invalid);}
    let id=string(&settings,"id")?;
    if id.is_empty() || id.len()>128 || !id.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_')) {return Err(Error::Invalid);}
    let provider=string(&settings,"provider")?;
    if !duduclaw_core::provider_env::KNOWN_PROVIDER_IDS.contains(&provider) {return Err(Error::Invalid);}
    let priority=integer(&settings,"priority",1)?;
    let budget=integer(&settings,"monthly_budget_cents",5000)?;
    if priority<0 || priority>u32::MAX as i64 || budget<0 {return Err(Error::Invalid);}
    if !item.fields.iter().any(|f|f.name=="credential" && matches!(f.value,ProposalValue::SecretFingerprint {..})) {return Err(Error::Invalid);}
    Ok(AccountInput {id:id.into(),provider:provider.into(),priority:priority as u32,budget:budget as u64})
}
pub(super) fn merge_account(home:&Path,table:&mut toml::Table,item:&ConfirmationItem,secret:&str,encrypt:bool)->Result<()> {
    let input=account_input(item)?;
    safe_loader_inputs(table)?;
    crate::product_policy::require_provider(home,&input.provider).map_err(|_|Error::Denied)?;
    let encrypted=if encrypt {Some(crate::config_crypto::encrypt_value(secret,home).ok_or(Error::Storage)?)} else {None};
    let entry=super::super::accounts_util::build_account_entry(&input.id,"api_key",&input.provider,input.budget,input.priority as u64,
        secret,encrypted.as_deref());
    let accounts=table.entry("accounts").or_insert_with(||toml::Value::Array(vec![])).as_array_mut().ok_or(Error::Invalid)?;
    let indexes:Vec<usize>=accounts.iter().enumerate().filter(|(_,a)|a.get("id").and_then(toml::Value::as_str)==Some(input.id.as_str())).map(|(i,_)|i).collect();
    if indexes.len()>1 {return Err(Error::Invalid);}
    if let Some(index)=indexes.first() {accounts[*index]=toml::Value::Table(entry.table);} else {accounts.push(toml::Value::Table(entry.table));}
    Ok(())
}
/// Rotation edits the current credential in place. It must not reapply old
/// proposal defaults over intervening normal account edits (budget, tags, label).
pub(super) fn rotate_account_key(home:&Path,table:&mut toml::Table,item:&ConfirmationItem,secret:&str)->Result<()> {
    let input=account_input(item)?;
    safe_loader_inputs(table)?;
    crate::product_policy::require_provider(home,&input.provider).map_err(|_|Error::Denied)?;
    let encrypted=crate::config_crypto::encrypt_value(secret,home).ok_or(Error::Storage)?;
    let template=super::super::accounts_util::build_account_entry(&input.id,"api_key",&input.provider,
        input.budget,input.priority as u64,secret,Some(&encrypted));
    let accounts=table.get_mut("accounts").and_then(toml::Value::as_array_mut).ok_or(Error::Invalid)?;
    let indexes:Vec<usize>=accounts.iter().enumerate().filter(|(_,a)|a.get("id").and_then(toml::Value::as_str)==Some(input.id.as_str())).map(|(i,_)|i).collect();
    if indexes.len()!=1 {return Err(Error::Stale);}
    let account=accounts[indexes[0]].as_table_mut().ok_or(Error::Invalid)?;
    if string(account,"type")?!="api_key" || string(account,"provider")?!=input.provider {return Err(Error::Stale);}
    account.remove(template.key_field);
    account.insert(format!("{}_enc",template.key_field),toml::Value::String(encrypted));
    Ok(())
}
pub(super) fn find_account(table:&toml::Table,id:&str)->Result<Option<toml::Table>> {
    let Some(accounts)=table.get("accounts") else {return Ok(None);};
    let accounts=accounts.as_array().ok_or(Error::Invalid)?;
    let matches:Vec<_>=accounts.iter().filter(|a|a.get("id").and_then(toml::Value::as_str)==Some(id)).collect();
    if matches.len()>1 {return Err(Error::Invalid);}
    matches.first().map(|a|a.as_table().cloned().ok_or(Error::Invalid)).transpose()
}
pub(super) fn read_account_secret(account:&toml::Table,home:&Path)->Result<Secret> {
    // The existing writer owns provider-specific field naming.
    let sample=super::super::accounts_util::build_account_entry("readback","api_key",string(account,"provider")?,0,1,"placeholder",Some("placeholder"));
    let encrypted=account.get(&format!("{}_enc",sample.key_field)).and_then(toml::Value::as_str).ok_or(Error::Denied)?;
    if account.contains_key(sample.key_field) {return Err(Error::Denied);}
    SecretRef::classify(Some(encrypted),None).resolve_sync(home).ok_or(Error::Denied)
}
fn safe_loader_inputs(table:&toml::Table)->Result<()> {
    if config_commit::toml_at(table,"account_loading.inherit_host_credentials").and_then(toml::Value::as_bool)!=Some(false)
        || table.contains_key("api") {return Err(Error::Denied);}
    if let Some(accounts)=table.get("accounts") {
        let mut ids=std::collections::HashSet::new();
        for account in accounts.as_array().ok_or(Error::Invalid)? {
            let account=account.as_table().ok_or(Error::Invalid)?;
            if string(account,"type")?!="api_key" || !ids.insert(string(account,"id")?) {return Err(Error::Denied);}
            if !duduclaw_core::provider_env::KNOWN_PROVIDER_IDS.contains(&string(account,"provider")?) {return Err(Error::Denied);}
            let sample=super::super::accounts_util::build_account_entry("readback","api_key",string(account,"provider")?,0,1,"placeholder",Some("placeholder"));
            let encrypted=format!("{}_enc",sample.key_field);
            if account.keys().any(|k|config_raw_rpc::is_secret_key(k) && *k!=encrypted) {return Err(Error::Denied);}
            let enc=string(account,&encrypted)?;
            // Inline ciphertext only. Never permit env or remote secret lookup.
            if enc.is_empty() || enc.split_once("://").is_some() {return Err(Error::Denied);}
        }
    }
    Ok(())
}
fn string<'a>(table:&'a toml::Table,key:&str)->Result<&'a str> {table.get(key).and_then(toml::Value::as_str).ok_or(Error::Invalid)}
fn integer(table:&toml::Table,key:&str,default:i64)->Result<i64> {match table.get(key) {None=>Ok(default),Some(v)=>v.as_integer().ok_or(Error::Invalid)}}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_migration::ids::RequestId;
    struct FailFirstReadback {
        inner:std::sync::Arc<GatewayAccountReadback>,
        failed:std::sync::atomic::AtomicBool,
    }
    impl MigrationConfigReadback for FailFirstReadback {
        fn supports(&self,item:&ConfirmationItem)->bool {self.inner.supports(item)}
        fn reload_and_verify(&self,item:&ConfirmationItem,relative:&str,expected:&str)->Result<()> {
            if !self.failed.swap(true,std::sync::atomic::Ordering::SeqCst) {return Err(Error::Apply);}
            self.inner.reload_and_verify(item,relative,expected)
        }
    }
    fn proposal(provider:&str)->ConfirmationItem {
        ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:RequestId([4;16]),revision:1},proposal_digest:Sha256Digest([5;32])},
            category:ConfirmationCategory::ConfigurationSection,status:ConfirmationStatus::Applying,high_risk:true,
            fields:vec![ProposalField {name:"section".into(),value:ProposalValue::Text {value:"accounts".into()}},
                ProposalField {name:"settings".into(),value:ProposalValue::Text {value:format!("id='migration-test'\ntype='api_key'\nprovider='{provider}'\napi_key='«set»'\npriority=3\nmonthly_budget_cents=2000\n")}},
                ProposalField {name:"credential".into(),value:ProposalValue::SecretFingerprint {keyed_fingerprint:Sha256Digest([9;32]),needs_rotation:true}}]}
    }
    #[test]
    fn migration_authority_accounts_schema_refuses_unknown_fields_and_implicit_provider() {
        let good=proposal("openai");assert!(GatewayAccountReadback::item_supported(&good));
        let mut bad=good.clone();
        if let ProposalValue::Text {value}=&mut bad.fields[1].value {value.push_str("extra='ignored'\n");}
        assert!(!GatewayAccountReadback::item_supported(&bad));
        assert!(!GatewayAccountReadback::item_supported(&proposal("unknown-provider")));
        let home=tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),"").unwrap();
        assert!(!GatewayAccountReadback::available(home.path(),&good));
        std::fs::write(home.path().join("config.toml"),"[account_loading]\ninherit_host_credentials=false\n").unwrap();
        assert!(GatewayAccountReadback::available(home.path(),&good));
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn migration_authority_accounts_real_rotator_reads_rotated_key_and_compensation() {
        // One test drives both providers sequentially because production's
        // existing rotator cache is global; no provider endpoint is called.
        for provider in ["openai","gemini"] {
            let home=tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.toml"),"[account_loading]\ninherit_host_credentials=false\n").unwrap();
            let handle=tokio::runtime::Handle::current();
            let home_path=home.path().to_path_buf();
            let p=proposal(provider);
            tokio::task::spawn_blocking(move || {
                let runtime=std::sync::Arc::new(GatewayAccountReadback::new(home_path.clone(),handle));
                let mut authority=GatewayAuthority::new(home_path.clone()).unwrap().with_runtime(runtime.clone());
                let mut p=p;
                p.fields[2].value=authority.fingerprint_secret("OLD-ACCOUNT-CANARY").unwrap();
                let mut store=duduclaw_migration::confirmation::ConfirmationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();
                p=store.stage_proposal(p.binding.operation_id,p.category,p.fields,&authority).unwrap();
                authority.stage_legacy_secret(&p,"OLD-ACCOUNT-CANARY").unwrap();
                let session=duduclaw_migration::confirmation::ApprovalSession::from_verified_session(true,true,Some(100),[1;32],RequestId([2;16]));
                store.approve(&[p.binding],&session,[1;32],100,&authority).unwrap();
                for _ in 0..3 {p=store.apply_next(&p.binding,&mut authority).unwrap();}
                assert_eq!(p.status,ConfirmationStatus::Applied);
                assert!(store.incomplete().unwrap());
                let persisted=std::fs::read_to_string(home_path.join("config.toml")).unwrap();
                assert!(!persisted.contains("OLD-ACCOUNT-CANARY"));
                p.status=ConfirmationStatus::Applied;
                // Simulate a normal account settings save after migration apply.
                // Rotation must preserve this newer metadata, not the proposal.
                let mut edited:toml::Table=persisted.parse().unwrap();
                let entry=edited.get_mut("accounts").unwrap().as_array_mut().unwrap()[0].as_table_mut().unwrap();
                entry.insert("monthly_budget_cents".into(),toml::Value::Integer(700));
                entry.insert("priority".into(),toml::Value::Integer(9));
                entry.insert("label".into(),toml::Value::String("Changed after migration".into()));
                entry.insert("tags".into(),toml::Value::Array(vec![toml::Value::String("finance".into())]));
                let expected_metadata=entry.clone();
                let edited_text=config_commit::render_preserving(&persisted,&edited).unwrap();
                config_commit::commit_text_locked(&home_path.join("config.toml"),&config_commit::content_hash(&persisted),&edited_text).unwrap();
                // Inject failure after the config commit but before cache readback.
                // The old obligation remains, and the same new value can retry.
                authority=authority.with_runtime(std::sync::Arc::new(FailFirstReadback {
                    inner:runtime,failed:std::sync::atomic::AtomicBool::new(false),
                }));
                assert!(authority.rotate_secret(&p,"NEW-ACCOUNT-CANARY").is_err());
                let committed:toml::Table=std::fs::read_to_string(home_path.join("config.toml")).unwrap().parse().unwrap();
                assert_eq!(read_account_secret(&find_account(&committed,"migration-test").unwrap().unwrap(),&home_path).unwrap().expose(),"NEW-ACCOUNT-CANARY");
                assert!(store.incomplete().unwrap());
                let receipt=authority.rotate_secret(&p,"NEW-ACCOUNT-CANARY").unwrap();
                assert!(store.incomplete().unwrap());
                store.record_rotation(&p.binding,"credential",receipt,&authority).unwrap();
                assert!(!store.incomplete().unwrap());
                authority.verify_rotated(&p.binding,"credential",receipt).unwrap();
                assert!(!std::fs::read_to_string(home_path.join("config.toml")).unwrap().contains("NEW-ACCOUNT-CANARY"));
                assert_eq!(authority.rotate_secret(&p,"NEW-ACCOUNT-CANARY").unwrap(),receipt);
                let live:toml::Table=std::fs::read_to_string(home_path.join("config.toml")).unwrap().parse().unwrap();
                let mut actual=find_account(&live,"migration-test").unwrap().unwrap();
                let mut expected_metadata=expected_metadata;
                let template=super::super::super::accounts_util::build_account_entry("migration-test","api_key",provider,0,1,"unused",Some("unused"));
                actual.remove(&format!("{}_enc",template.key_field));
                expected_metadata.remove(&format!("{}_enc",template.key_field));
                assert_eq!(actual,expected_metadata);
                // A rotation changes the protected live artifact. The old
                // application plan must not silently undo that later write.
                assert!(authority.disable_and_read_back(&p).is_err());
            }).await.unwrap();
            crate::claude_runner::invalidate_rotator_cache().await;
        }
        let home=tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),"[account_loading]\ninherit_host_credentials=false\n").unwrap();
        let path=home.path().to_path_buf();let handle=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let runtime=std::sync::Arc::new(GatewayAccountReadback::new(path.clone(),handle));
            let mut a=GatewayAuthority::new(path).unwrap().with_runtime(runtime);
            let mut p=proposal("openai");p.fields[2].value=a.fingerprint_secret("DISABLE-CANARY").unwrap();
            a.stage_legacy_secret(&p,"DISABLE-CANARY").unwrap();a.stage(&p).unwrap();a.publish(&p).unwrap();a.read_back(&p).unwrap();
            a.disable_and_read_back(&p).unwrap();
        }).await.unwrap();
        crate::claude_runner::invalidate_rotator_cache().await;
    }
}
