//! Gateway-owned meeting jobs. MCP callers never open the write store.
pub mod http;
mod store;
pub(crate) mod notice;
pub use notice::Binding as NotificationBinding;
pub use store::Job;
use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc, time::Duration};
use duduclaw_inference::transcription::{config::TranscriptionConfig, pipeline};
use store::Store;

pub struct Manager { pub home: PathBuf, store: Store }
impl Manager {
    pub fn open(home: PathBuf) -> Result<Arc<Self>,String> {
        if !duduclaw_core::gateway_instance::held(&home) {return Err("會議工作只能由主要閘道處理。".into());}
        let store=Store::open(&home)?;
        Ok(Arc::new(Self{home,store}))
    }
    pub fn agent_dir(&self, agent: &str) -> PathBuf {
        crate::ephemeral::resolve_agent_dir(&self.home,agent).unwrap_or_else(||self.home.join("agents").join(agent))
    }
    pub fn employee(&self, agent: &str) -> Result<(),String> {
        if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent) || agent==duduclaw_core::UNTRUSTED_AGENT_ID {return Err("無法驗證員工身分。".into());}
        let dir=self.agent_dir(agent);
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m|m.file_type().is_dir()) {return Err("找不到這位員工。".into());}
        let raw=duduclaw_core::agent_toml::read_effective_text(&dir).map_err(|_|"無法讀取員工設定。")?;
        let table=raw.parse::<toml::Table>().map_err(|_|"員工設定格式錯誤。")?;
        if let Some(caps)=table.get("capabilities") {
            let caps=caps.as_table().ok_or("員工權限設定格式錯誤。")?;
            for name in ["allowed_tools","denied_tools","scoped_tools","approval_required_tools","irreversible_tools","maybe_irreversible_tools"] {
                if let Some(value)=caps.get(name) {
                    if !value.as_array().is_some_and(|a|a.iter().all(|v|v.as_str().is_some())) {return Err("員工權限清單格式錯誤。".into());}
                }
            }
            let _:duduclaw_core::types::CapabilitiesConfig=toml::Value::Table(caps.clone()).try_into().map_err(|_|"員工權限設定格式錯誤。")?;
        }
        Ok(())
    }
    pub fn create(&self, agent: &str, source: &str, engine: Option<&str>, language: &str, turn: Option<&str>) -> Result<Job,String> {
        self.admit_job(agent,source,engine,language,turn)?.commit()
    }
    pub fn admit_job(&self, agent: &str, source: &str, engine: Option<&str>, language: &str, turn: Option<&str>) -> Result<Admission<'_>,String> {
        let job=self.build_job(agent,source,engine,language,turn)?;
        self.store.create(&job,job.config.staging_quota_bytes)?;
        self.snapshot_admission(job)
    }
    pub fn create_uploaded(&self, agent: &str, source: &str, upload_id: &str) -> Result<Job,String> {
        self.prepare_uploaded(agent,source,upload_id,None)?.commit()
    }
    pub fn create_uploaded_with_binding(&self, agent: &str, source: &str, upload_id: &str, binding: Option<NotificationBinding>) -> Result<Job,String> {
        self.prepare_uploaded(agent,source,upload_id,binding)?.commit()
    }
    pub fn prepare_uploaded(&self, agent: &str, source: &str, upload_id: &str, binding: Option<NotificationBinding>) -> Result<Admission<'_>,String> {
        if uuid::Uuid::parse_str(upload_id).is_err() {return Err("上傳識別錯誤。".into());}
        let mut job=self.build_job(agent,source,None,"zh-TW",None)?;
        job.source_upload_id=Some(upload_id.into());
        if let Some(binding)=binding {
            job.reply_channel=binding.reply_channel;job.reply_target=binding.reply_target;
            job.requester=binding.requester;job.session=binding.session;
        }
        // Two source-sized reservations cover import + snapshot during admission,
        // then snapshot + preparation copy after the imported file is removed.
        self.store.create_from_upload(&job,job.config.staging_quota_bytes,upload_id)?;
        let admission=self.snapshot_admission(job)?;
        pipeline::remove_attachment(&self.agent_dir(agent),Path::new(&admission.job.source))?;
        Ok(admission)
    }
    fn snapshot_admission(&self, job: Job) -> Result<Admission<'_>,String> {
        let mut admission=Admission{manager:self,job,committed:false};
        let staging=self.staging(&admission.job);
        std::fs::create_dir_all(&staging).map_err(|_|"無法建立會議暫存。")?;
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&staging,std::fs::Permissions::from_mode(0o700)).map_err(|_|"無法保護會議暫存。")?; }
        pipeline::preflight_output(&staging)?;
        let extension=Path::new(&admission.job.source).extension().and_then(|s|s.to_str()).unwrap_or("wav");
        let snapshot=staging.join(format!("approved.{extension}"));
        let (digest,size)=pipeline::snapshot_attachment(&self.agent_dir(&admission.job.agent),Path::new(&admission.job.source),&snapshot,admission.job.source_bytes)?;
        admission.job=self.store.update(&admission.job.agent,&admission.job.id,|j| {
            j.source_bytes=size;j.source_sha256=digest;j.snapshot_path=snapshot.to_string_lossy().into();Ok(())
        })?;
        Ok(admission)
    }
    pub fn list(&self, agent: &str) -> Result<Vec<Job>,String> { self.store.list(agent) }
    pub fn get(&self, agent: &str, id: &str) -> Result<Job,String> { self.store.get(agent,id) }
    pub fn cancel(&self, agent: &str, id: &str) -> Result<Job,String> {
        let result=self.store.cancel(agent,id);
        audit(&self.home,"meeting_job_cancel",agent,agent,id,if result.is_ok(){"allowed"}else{"refused"},None,None);
        result
    }
    fn build_job(&self, agent: &str, source: &str, engine: Option<&str>, language: &str, turn: Option<&str>) -> Result<Job,String> {
        self.employee(agent)?;
        self.background_allowed(agent)?;
        if language.is_empty() || language.len()>32 || !language.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-') {return Err("語言提示格式錯誤。".into());}
        let config=TranscriptionConfig::load(&self.home)?;
        config.select_engine(engine)?;
        let path=pipeline::validate_attachment(&self.agent_dir(agent),Path::new(source),config.max_bytes)?;
        let source_bytes=std::fs::metadata(&path).map_err(|_|"無法讀取錄音。")?.len();
        let now=chrono::Utc::now().timestamp();
        let binding=notice::capture(agent,turn);
        let job=Job{id:uuid::Uuid::new_v4().to_string(),agent:agent.into(),source:path.to_string_lossy().into(),source_bytes,source_sha256:String::new(),snapshot_path:String::new(),source_upload_id:None,
            config:config.clone(),language:language.into(),state:"admitting".into(),chunk:0,chunks:0,created_at:now,updated_at:now,
            reply_channel:binding.reply_channel,reply_target:binding.reply_target,requester:binding.requester,session:binding.session,
            result:None,error:None,cost_millicents:0,cost_estimated:false,reserved_bytes:source_bytes.checked_mul(2).and_then(|n|n.checked_add(store::CHUNK_RESERVE)).ok_or("會議大小超出範圍。")?,cleaned:false,notified:false,policy_digest:self.policy_digest(agent)?,notice_attempts:0,next_notice_at:0,notice_error:None};
        Ok(job)
    }
    fn staging(&self, job: &Job) -> PathBuf {self.home.join("meeting-staging").join(&job.id)}
    fn cleanup(&self, job: &Job) -> Result<(),String> {
        // Only server-generated UUIDs are ever used as staging components.
        if uuid::Uuid::parse_str(&job.id).is_err() {return Err("會議暫存識別錯誤。".into());}
        let dir=self.staging(job);
        if dir.exists() { std::fs::remove_dir_all(&dir).map_err(|_|"無法清理會議暫存。")?; }
        if job.terminal() {
            if let Some(upload)=&job.source_upload_id {
                if uuid::Uuid::parse_str(upload).is_err() {return Err("上傳識別錯誤。".into());}
                let source=Path::new(&job.source);
                let expected=self.agent_dir(&job.agent).join("attachments/meetings");
                if source.parent()!=Some(expected.as_path()) || source.file_stem().and_then(|s|s.to_str())!=Some(upload.as_str()) {return Err("上傳附件路徑不正確。".into());}
                pipeline::remove_attachment(&self.agent_dir(&job.agent),source)?;
            }
            if job.state!="done" {
                for ext in ["md","json"] {
                    pipeline::remove_attachment(&self.agent_dir(&job.agent),&self.agent_dir(&job.agent).join("attachments").join(format!("transcript-{}.{ext}",job.id)))?;
                }
            }
        }
        Ok(())
    }
    fn recover_staging(&self)->Result<(),String> {
        for j in self.store.pending()? {
            if !j.terminal() {
                let staging=self.staging(&j);
                if staging.exists() {
                    for entry in std::fs::read_dir(&staging).map_err(|_|"無法讀取會議暫存。")? {
                        let entry=entry.map_err(|_|"無法讀取會議暫存。")?;
                        // Preserve only the admission snapshot; remove crash-left scratch directories.
                        if entry.path()==Path::new(&j.snapshot_path) {continue;}
                        if entry.file_type().map_err(|_|"無法確認會議暫存。")?.is_dir() {std::fs::remove_dir_all(entry.path()).map_err(|_|"無法清理會議暫存。")?;}
                    }
                }
            }
        }
        self.store.requeue()
    }
    pub fn spawn(self: &Arc<Self>) {
        let m=self.clone();
        tokio::spawn(async move {
            // Reconciliation only runs once, under the process-wide gateway lock.
            if !duduclaw_core::gateway_instance::held(&m.home) {return;}
            let recovered=m.recover_staging();
            if recovered.is_err() {tracing::error!("meeting job recovery failed; worker stopped");return;}
            let mut notifying: HashMap<String,tokio::task::JoinHandle<()>>=HashMap::new();
            let mut running: HashMap<String,tokio::task::JoinHandle<()>>=HashMap::new();
            loop {
                if !duduclaw_core::gateway_instance::held(&m.home) {return;}
                let finished:Vec<_>=running.iter().filter(|(_,h)|h.is_finished()).map(|(id,_)|id.clone()).collect();
                for id in finished {if let Some(h)=running.remove(&id) {if h.await.is_err(){tracing::error!(job=%id,"meeting worker panicked; restart required");}}}
                notifying.retain(|_,h|!h.is_finished());
                if let Ok(jobs)=m.store.pending() {
                    let config=TranscriptionConfig::load(&m.home);
                    for job in jobs {
                        if running.contains_key(&job.id) {continue;}
                        if job.terminal() {
                            if !job.cleaned {
                                if m.cleanup(&job).is_err(){continue;}
                                let _=m.store.update(&job.agent,&job.id,|j|{j.cleaned=true;j.reserved_bytes=j.result.as_ref().and_then(|r|r["output_bytes"].as_u64()).unwrap_or(0);Ok(())});
                            }
                            if !job.notified && job.notice_attempts<5 && job.next_notice_at<=chrono::Utc::now().timestamp() && !notifying.contains_key(&job.id) && notifying.len()<2 {
                                let m=m.clone(); let j=job.clone();
                                notifying.insert(job.id.clone(),tokio::spawn(async move {
                                    let result=notice::deliver(&m.home,&j).await;
                                    let _=m.store.update(&j.agent,&j.id,|row|{
                                        row.notice_attempts+=1;
                                        row.next_notice_at=chrono::Utc::now().timestamp()+60*(1i64<<row.notice_attempts.min(5));
                                        match result {Ok(())=>{row.notified=true;row.notice_error=None;},Err(e)=>{row.notice_error=Some(e);if row.notice_attempts>=5 {row.notified=true;}}}
                                        Ok(())
                                    });
                                }));
                            }
                            continue;
                        }
                        let Ok(cfg)=&config else {continue;};
                        if job.state=="queued" && running.len()<cfg.max_concurrent_jobs as usize {
                            let claimed=m.store.update(&job.agent,&job.id,|j|{if j.state!="queued" {return Err("工作已變更。".into());} j.state="preparing".into();Ok(())});
                            if let Ok(job)=claimed {let m=m.clone();running.insert(job.id.clone(),tokio::spawn(async move {m.run(job).await;}));}
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
    async fn run(self: Arc<Self>, job: Job) {
        let result=async {
            self.employee(&job.agent)?;
            let mut cfg=self.current_config(&job)?;
            cfg.max_bytes=cfg.max_bytes.min(job.source_bytes);
            let source=PathBuf::from(&job.snapshot_path);
            if source.parent()!=Some(self.staging(&job).as_path()) {return Err("會議快照路徑無效，請重新提交。".into());}
            pipeline::verify_snapshot(&source,&job.source_sha256,job.source_bytes)?;
            if job.source_upload_id.is_some() {pipeline::remove_attachment(&self.agent_dir(&job.agent),Path::new(&job.source))?;}
            let staging=self.staging(&job);
            std::fs::create_dir_all(&staging).map_err(|_|"無法建立會議暫存。")?;
            pipeline::preflight_output(&staging)?;
            let observer=Observer{manager:self.clone(),job:job.clone()};
            crate::meeting_transcription::execute(&self.home,&job.agent,&source,&self.agent_dir(&job.agent).join("attachments"),
                &cfg,cfg.engine,&job.language,Some(&observer),Some(&job.id),job.session.as_deref(),Some(&staging)).await
        }.await;
        let _=self.store.update(&job.agent,&job.id,|j|{
            if j.state=="cancelled" {return Ok(());}
            match result {Ok(value)=>{j.state="done".into();j.result=Some(value);},Err(error)=>{j.state="failed".into();j.error=Some(duduclaw_core::truncate_chars(&error,500).to_string());}}
            Ok(())
        });
    }
    pub fn background_allowed(&self, agent:&str)->Result<(),String> {
        duduclaw_core::autonomy_gate::check(&self.home,agent,duduclaw_core::autonomy_gate::AutonomyPath::Background)
            .map_err(|_|"這位員工的背景工作已暫停。".into())
    }
    pub fn policy_digest(&self,agent:&str)->Result<String,String> {
        use sha2::{Digest,Sha256};
        let employee=duduclaw_core::agent_toml::read_effective_text(&self.agent_dir(agent)).map_err(|_|"無法讀取員工權限。")?;
        let employee:toml::Table=employee.parse().map_err(|_|"員工權限格式錯誤。")?;
        let global=match std::fs::read_to_string(self.home.join("config.toml")) {Ok(v)=>v,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>String::new(),Err(_)=>return Err("無法讀取權限設定。".into())};
        let global:toml::Table=global.parse().map_err(|_|"權限設定格式錯誤。")?;
        let policy=serde_json::json!({"capabilities":employee.get("capabilities"),"permissions":employee.get("permissions"),"agent_redaction":employee.get("redaction"),"redaction":global.get("redaction"),"security":global.get("security"),"action_review":global.get("action_review")});
        Ok(hex::encode(Sha256::digest(policy.to_string().as_bytes())))
    }
    fn current_config(&self, job: &Job) -> Result<TranscriptionConfig,String> {
        let cfg=TranscriptionConfig::load(&self.home)?;
        if cfg.max_bytes<job.source_bytes {return Err("錄音超過目前設定的大小上限。".into());}
        if cfg.engine!=job.config.engine || cfg.model!=job.config.model {return Err("轉錄設定已變更，請重新提交會議。".into());}
        Ok(cfg)
    }
}
struct Observer {manager:Arc<Manager>,job:Job}
#[async_trait::async_trait]
impl crate::meeting_transcription::JobObserver for Observer {
    fn source_binding(&self)->Option<(&str,u64)> {Some((&self.job.source_sha256,self.job.source_bytes))}
    async fn before_chunk(&self,index:usize,total:usize)->Result<(),String>{
        if !duduclaw_core::gateway_instance::held(&self.manager.home){return Err("閘道已停止處理會議。".into());}
        self.manager.employee(&self.job.agent)?;
        self.manager.background_allowed(&self.job.agent)?;
        if self.manager.policy_digest(&self.job.agent)?!=self.job.policy_digest {return Err("員工權限已變更，請重新提交會議。".into());}
        self.manager.current_config(&self.job)?;
        crate::computer_use_sessions::gates::tool_gates(&self.manager.home,&self.job.agent,"meeting_transcribe").await.map_err(|e|e.message)?;
        self.manager.background_allowed(&self.job.agent)?;
        self.manager.current_config(&self.job)?;
        if self.manager.policy_digest(&self.job.agent)?!=self.job.policy_digest {return Err("員工權限已變更，請重新提交會議。".into());}
        self.manager.store.update(&self.job.agent,&self.job.id,|j|{
            if j.state=="cancelled" {return Err("會議轉錄已取消。".into());}
            if total>0 {j.state="transcribing".into();j.chunk=index;j.chunks=total;}
            Ok(())
        })?; Ok(())
    }
    async fn cost(&self,total:u64,estimated:bool)->Result<(),String>{
        self.manager.store.update(&self.job.agent,&self.job.id,|j|{j.cost_millicents=self.job.cost_millicents.saturating_add(total);j.cost_estimated|=estimated;Ok(())})?;Ok(())
    }
}

/// Reservations precede the copy and remain live throughout approval waits.
/// Dropping an uncommitted admission fails closed and releases its private data.
pub struct Admission<'a> { manager: &'a Manager, pub job: Job, committed: bool }
impl Admission<'_> {
    pub fn binding(&self) -> serde_json::Value { serde_json::json!({"source_sha256":self.job.source_sha256,"source_bytes":self.job.source_bytes}) }
    pub fn commit(self) -> Result<Job,String> { self.commit_checked(||Ok(())) }
    pub fn commit_checked(mut self, check: impl FnOnce()->Result<(),String>) -> Result<Job,String> {
        pipeline::verify_snapshot(Path::new(&self.job.snapshot_path),&self.job.source_sha256,self.job.source_bytes)?;
        let job=self.manager.store.update(&self.job.agent,&self.job.id,|j| {
            // Re-read authorization after hashing and after acquiring the write transaction.
            check()?;
            if j.state!="admitting" {return Err("會議提交狀態已變更。".into());} j.state="queued".into();Ok(())
        })?;
        self.committed=true;
        audit(&self.manager.home,"meeting_job_create",job.requester.as_deref().unwrap_or(&job.agent),&job.agent,&job.id,"approved",Some(job.config.engine.as_str()),job.config.model.as_deref());
        Ok(job)
    }
}
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.committed {return;}
        let mut job=self.job.clone();job.state="failed".into();
        let cleaned=self.manager.cleanup(&job).is_ok();
        let _=self.manager.store.update(&job.agent,&job.id,|j| {j.state="failed".into();j.error=Some("會議提交未獲核准或驗證失敗，請重新提交。".into());j.notified=true;j.cleaned=cleaned;if cleaned {j.reserved_bytes=0;}Ok(())});
        audit(&self.manager.home,"meeting_job_create",&job.agent,&job.agent,&job.id,"admission_refused",None,None);
    }
}

/// Only identifiers and fixed outcomes belong in this audit; never pass content or paths.
pub(crate) fn audit(home:&Path,event:&str,actor:&str,agent:&str,id:&str,outcome:&str,provider:Option<&str>,model:Option<&str>) {
    use sha2::{Digest,Sha256};
    let prefix=|value:&str|hex::encode(Sha256::digest(value.as_bytes()))[..12].to_owned();
    duduclaw_security::audit::append_audit_event(home,&duduclaw_security::audit::AuditEvent::new(event,agent,
        duduclaw_security::audit::Severity::Info,serde_json::json!({"actor_hash":prefix(actor),"employee":agent,"id_hash":prefix(id),"outcome":outcome,"provider":provider,"model":model})));
}

#[cfg(test)] mod tests {
    use super::*;
    fn setup()->(tempfile::TempDir,Arc<Manager>) {
        let home=tempfile::tempdir().unwrap();let path=home.path().canonicalize().unwrap();
        std::fs::create_dir_all(path.join("agents/alice/attachments")).unwrap();
        std::fs::write(path.join("agents/alice/agent.toml"),"[agent]\nname='alice'\nstatus='active'\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n").unwrap();
        // Disable turn-taint lookup: this fixture has no process-global turn ledger.
        std::fs::write(path.join("config.toml"), "[security]\negress_guard='off'\n").unwrap();
        // Fully local autonomy state: no HOME/env override or process-wide pause.
        std::fs::write(path.join(duduclaw_core::autonomy_gate::INIT_FILE), b"").unwrap();
        std::fs::write(path.join(duduclaw_core::autonomy_gate::STATE_FILE),
            r#"{"version":1,"global":null,"agents":{}}"#).unwrap();
        duduclaw_core::autonomy_gate::check(&path,"alice",duduclaw_core::autonomy_gate::AutonomyPath::Background).unwrap();
        duduclaw_core::gateway_instance::acquire(&path).unwrap();
        let manager=Manager::open(path).unwrap();(home,manager)
    }
    #[test] fn meeting_background_fixture_keeps_missing_unknown_and_paused_state_closed() {
        let (_home,m)=setup();
        assert!(m.background_allowed("alice").is_ok());
        let agent=m.agent_dir("alice").join("agent.toml");
        let original=std::fs::read_to_string(&agent).unwrap();
        for config in [original.replace("status='active'", ""), original.replace("status='active'", "status='unknown'"), original.replace("status='active'", "status='paused'")] {
            std::fs::write(&agent,config).unwrap();
            assert!(m.background_allowed("alice").is_err());
        }
        std::fs::write(&agent,original).unwrap();
        std::fs::remove_file(m.home.join(duduclaw_core::autonomy_gate::STATE_FILE)).unwrap();
        assert!(m.background_allowed("alice").is_err());
        std::fs::write(m.home.join(duduclaw_core::autonomy_gate::STATE_FILE), "unknown").unwrap();
        assert!(m.background_allowed("alice").is_err());
    }
    #[test] fn admission_binds_bytes_before_approval_and_keeps_snapshot_after_replacement() {
        let (_home,m)=setup();let source=m.agent_dir("alice").join("attachments/audio.wav");
        std::fs::write(&source,b"approved").unwrap();
        let admission=m.admit_job("alice",source.to_str().unwrap(),None,"zh-TW",None).unwrap();
        assert_eq!(admission.job.state,"admitting");
        assert_eq!(admission.binding()["source_bytes"],8);
        assert_eq!(admission.binding()["source_sha256"].as_str().unwrap().len(),64);
        std::fs::remove_file(&source).unwrap();std::fs::write(&source,b"replaced").unwrap();
        let job=admission.commit().unwrap();
        assert_eq!(job.state,"queued");
        assert_eq!(std::fs::read(&job.snapshot_path).unwrap(),b"approved");
        pipeline::verify_snapshot(Path::new(&job.snapshot_path),&job.source_sha256,job.source_bytes).unwrap();
        assert!(pipeline::verify_snapshot(&source,&job.source_sha256,job.source_bytes).is_err());
    }
    #[test] fn restart_keeps_admission_snapshot_but_removes_crashed_preparation() {
        let (_home,m)=setup();let source=m.agent_dir("alice").join("attachments/audio.wav");std::fs::write(&source,b"approved").unwrap();
        let job=m.create("alice",source.to_str().unwrap(),None,"zh-TW",None).unwrap();
        m.store.update("alice",&job.id,|j|{j.state="transcribing".into();Ok(())}).unwrap();
        let scratch=m.staging(&job).join("duduclaw-transcription-crashed");std::fs::create_dir(&scratch).unwrap();std::fs::write(scratch.join("source.wav"),b"scratch").unwrap();
        m.recover_staging().unwrap();
        assert!(!scratch.exists());assert!(Path::new(&job.snapshot_path).exists());assert_eq!(m.get("alice",&job.id).unwrap().state,"queued");
    }
    #[test] fn final_authorization_after_snapshot_verification_can_refuse_admission() {
        let (_home,m)=setup();let source=m.agent_dir("alice").join("attachments/audio.wav");std::fs::write(&source,b"approved").unwrap();
        let admission=m.admit_job("alice",source.to_str().unwrap(),None,"zh-TW",None).unwrap();
        let id=admission.job.id.clone();let snapshot=admission.job.snapshot_path.clone();
        let checked=std::cell::Cell::new(false);
        assert!(admission.commit_checked(|| { checked.set(true);Err("權限已降為檢視者。".into()) }).is_err());
        assert!(checked.get());assert_eq!(m.get("alice",&id).unwrap().state,"failed");assert!(!Path::new(&snapshot).exists());
        let admission=m.admit_job("alice",source.to_str().unwrap(),None,"zh-TW",None).unwrap();
        std::fs::remove_file(&admission.job.snapshot_path).unwrap();std::fs::write(&admission.job.snapshot_path,b"replaced").unwrap();
        let checked=std::cell::Cell::new(false);
        assert!(admission.commit_checked(|| { checked.set(true);Ok(()) }).is_err());
        assert!(!checked.get(),"snapshot verification must precede the final live authorization read");
    }
    #[test] fn abandoned_admission_releases_reserved_bytes_and_snapshot() {
        let (_home,m)=setup();let source=m.agent_dir("alice").join("attachments/audio.wav");std::fs::write(&source,b"approved").unwrap();
        let admission=m.admit_job("alice",source.to_str().unwrap(),None,"zh-TW",None).unwrap();
        let id=admission.job.id.clone();let snapshot=admission.job.snapshot_path.clone();drop(admission);
        let job=m.get("alice",&id).unwrap();assert_eq!(job.state,"failed");assert_eq!(job.reserved_bytes,0);assert!(!Path::new(&snapshot).exists());assert!(source.exists());
    }
    #[test] fn audit_hashes_actor_and_record_identity_without_sensitive_content() {
        let home=tempfile::tempdir().unwrap();
        audit(home.path(),"meeting_job_create","secret-actor","alice","secret-token","approval_refused",Some("gemini"),Some("model"));
        let text=std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(!text.contains("secret-actor"));assert!(!text.contains("secret-token"));assert!(text.contains("approval_refused"));
    }
}
