//! Bounded resumable uploads. Dashboard and capability-link routes share only
//! this already-authorized owner/employee service; authentication stays outside.
mod store;
pub(crate) mod traffic;
pub(crate) mod http;
pub use http::{router,Authenticate};
pub use store::{CHUNK_SIZE,Upload};
use std::{path::PathBuf,sync::Arc,io::{Seek,SeekFrom,Write}};
use axum::body::Bytes;
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use duduclaw_inference::transcription::{config::TranscriptionConfig,pipeline};

struct MutationAudit<'a>{home:&'a std::path::Path,event:&'static str,owner:&'a str,agent:&'a str,id:String,outcome:&'static str}
impl Drop for MutationAudit<'_>{fn drop(&mut self){crate::meeting_jobs::audit(self.home,self.event,self.owner,self.agent,&self.id,self.outcome,None,None);}}

pub struct Service {pub manager:Arc<crate::meeting_jobs::Manager>,store:store::Store,gate:tokio::sync::Mutex<()>}
impl Service {
    pub fn open(manager:Arc<crate::meeting_jobs::Manager>)->Result<Arc<Self>,String>{
        let store=store::Store::open(&manager.home)?;
        let service=Arc::new(Self{manager,store,gate:tokio::sync::Mutex::new(())});
        let worker=service.clone();
        tokio::spawn(async move {loop{
            if !duduclaw_core::gateway_instance::held(&worker.manager.home){return;}
            {let _gate=worker.gate.lock().await;let _=worker.sweep();}
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }});
        Ok(service)
    }
    fn root(&self)->PathBuf{self.manager.home.join("meeting-upload-staging")}
    fn staged(&self,id:&str)->Result<PathBuf,String>{
        if uuid::Uuid::parse_str(id).is_err(){return Err(store::NOT_FOUND.into());}
        Ok(self.root().join(format!("{id}.part")))
    }
    fn output(&self,u:&Upload)->Result<PathBuf,String>{
        if !matches!(u.extension.as_str(),"mp4"|"webm"|"wav"|"mp3"|"flac"|"ogg"|"aac"|"aiff") || uuid::Uuid::parse_str(&u.id).is_err(){return Err("錄音格式尚未確認。".into());}
        Ok(self.manager.agent_dir(&u.agent).join("attachments/meetings").join(format!("{}.{}",u.id,u.extension)))
    }
    pub async fn create_for(&self,owner:&str,agent:&str,size:u64)->Result<Value,String>{
        self.create_checked_for(owner,agent,size,||Ok(())).await
    }
    pub async fn create_checked_for(&self,owner:&str,agent:&str,size:u64,check:impl Fn()->Result<(),String>)->Result<Value,String>{
        let mut audit=MutationAudit{home:&self.manager.home,event:"upload_create",owner,agent,id:"unallocated".into(),outcome:"admission_refused"};
        self.manager.employee(agent)?;
        self.manager.background_allowed(agent)?;
        let admission_policy=self.manager.policy_digest(agent)?;
        self.admit(agent).await?;
        let cfg=TranscriptionConfig::load(&self.manager.home)?;
        if size==0 || size>cfg.max_bytes{return Err("請選擇大小在設定上限內的非空錄音或影片。".into());}
        if owner.is_empty() || owner.len()>256{return Err("無法驗證上傳者。".into());}
        let _gate=self.gate.lock().await;
        if self.manager.policy_digest(agent)?!=admission_policy {return Err("員工權限已變更，請重新上傳。".into());}
        check()?;
        self.sweep()?;
        std::fs::create_dir_all(self.root()).map_err(|_|"無法建立上傳暫存。")?;
        pipeline::preflight_output(&self.root())?;
        let u=self.store.create(agent,owner,size,cfg.staging_quota_bytes)?;
        audit.id=u.id.clone();
        let path=self.staged(&u.id)?;
        let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600).custom_flags(libc::O_NOFOLLOW);}
        let file=options.open(&path);
        match file.and_then(|f|{f.set_len(size)?;f.sync_all()}) {
            Ok(())=>{audit.outcome="allowed";Ok(u.receipt(Vec::new(),cfg.max_bytes))},
            Err(_)=>{let _=std::fs::remove_file(path);let _=self.store.expire(&u.id);Err("無法配置上傳暫存空間。".into())}
        }
    }
    pub fn get_for(&self,owner:&str,id:&str)->Result<Upload,String>{self.store.get(id,owner)}
    pub fn status_for(&self,owner:&str,id:&str)->Result<Value,String>{
        let u=self.get_for(owner,id)?;
        let mut receipt=u.receipt(self.store.received(id)?,TranscriptionConfig::load(&self.manager.home)?.max_bytes);
        receipt["hashes"]=self.store.hashes(id)?;
        Ok(receipt)
    }
    pub fn preflight_for(&self,owner:&str,id:&str,index:u64)->Result<usize,String>{
        let u=self.get_for(owner,id)?;
        if u.state!="uploading"{return Err("這次上傳已結束。".into());}
        let cfg=TranscriptionConfig::load(&self.manager.home)?;
        if u.size>cfg.max_bytes{return Err("檔案超過目前設定的上限。".into());}
        self.store.budget(cfg.staging_quota_bytes)?;
        u.expected(index)
    }
    pub async fn put_for(&self,owner:&str,id:&str,index:u64,hash:&str,bytes:Bytes)->Result<Value,String>{
        self.put_checked_for(owner,id,index,hash,bytes,||Ok(())).await
    }
    pub async fn put_checked_for(&self,owner:&str,id:&str,index:u64,hash:&str,bytes:Bytes,check:impl Fn()->Result<(),String>)->Result<Value,String>{
        let _gate=self.gate.lock().await;
        check()?;
        let expected=self.preflight_for(owner,id,index)?;
        if bytes.len()!=expected{return Err("上傳分段大小不符，請重試這一段。".into());}
        if hash.len()!=64 || !hash.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)) || hex::encode(Sha256::digest(&bytes))!=hash {
            return Err("上傳分段驗證失敗，請重試這一段。".into());
        }
        if let Some(existing)=self.store.hash(id,index)? {
            if existing!=hash{return Err("這一段已收到不同內容，請重新建立上傳。".into());}
            return self.status_for(owner,id);
        }
        let extension=if index==0{Some(sniff(&bytes).ok_or("檔案內容不是支援的音訊或影片。")?)}else{None};
        pipeline::preflight_output(&self.root())?;
        let mut options=std::fs::OpenOptions::new();options.write(true);
        #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.custom_flags(libc::O_NOFOLLOW);}
        let mut file=options.open(self.staged(id)?).map_err(|_|"無法寫入上傳分段。")?;
        let metadata=file.metadata().map_err(|_|"無法讀取上傳暫存。")?;
        if !metadata.is_file(){return Err("上傳暫存無效。".into());}
        #[cfg(unix)]{use std::os::unix::fs::MetadataExt;if metadata.nlink()!=1{return Err("上傳暫存無效。".into());}}
        file.seek(SeekFrom::Start(index*CHUNK_SIZE)).and_then(|_|file.write_all(&bytes)).and_then(|_|file.sync_data()).map_err(|_|"無法儲存上傳分段。")?;
        self.store.record_chunk(id,index,hash,extension)?;
        self.status_for(owner,id)
    }
    pub async fn complete_for(&self,owner:&str,id:&str,binding:Option<crate::meeting_jobs::NotificationBinding>)->Result<Value,String>{
        self.complete_checked_for(owner,id,binding,||Ok(())).await
    }
    pub async fn complete_checked_for(&self,owner:&str,id:&str,binding:Option<crate::meeting_jobs::NotificationBinding>,check:impl Fn()->Result<(),String>)->Result<Value,String>{
        let _gate=self.gate.lock().await;
        let u=self.get_for(owner,id)?;
        let mut audit=MutationAudit{home:&self.manager.home,event:"upload_complete",owner,agent:&u.agent,id:id.into(),outcome:"completion_refused"};
        check()?;
        if let Some(job)=&u.job_id{audit.outcome="already_completed";return self.manager.get(&u.agent,job).map(|j|j.public());}
        let cfg=TranscriptionConfig::load(&self.manager.home)?;
        if u.size>cfg.max_bytes{return Err("檔案超過目前設定的上限。".into());}
        self.store.budget(cfg.staging_quota_bytes)?;
        let received=self.store.received(id)?;
        if received.len() as u64!=u.chunks() || received.iter().enumerate().any(|(i,n)|i as u64!=*n){return Err("仍有分段尚未上傳完成。".into());}
        self.manager.employee(&u.agent)?;
        self.admit(&u.agent).await?;
        check()?;
        let output=self.output(&u)?;
        let parent=output.parent().ok_or("無法建立會議附件。")?;
        let attachments=self.manager.agent_dir(&u.agent).join("attachments");
        std::fs::create_dir_all(&attachments).map_err(|_|"無法建立員工附件。")?;
        pipeline::preflight_output(&attachments)?;
        std::fs::create_dir_all(parent).map_err(|_|"無法建立會議附件。")?;
        pipeline::preflight_output(parent)?;
        self.store.completing(id)?;
        let staged=self.staged(id)?;
        if staged.exists(){
            // link + unlink is a no-clobber move on this gateway-home filesystem.
            // A restart between the two operations completes the same move.
            let existed=output.exists();
            if !existed{std::fs::hard_link(&staged,&output).map_err(|_|"無法完成會議附件。")?;}
            #[cfg(not(unix))]{
                // No portable stable file ID: a retry after the link operation
                // is refused rather than deleting an unverified source.
                if existed{return Err("會議附件狀態無法確認，請重新上傳。".into());}
            }
            #[cfg(unix)]{
                use std::os::unix::fs::MetadataExt;
                let a=std::fs::symlink_metadata(&staged).map_err(|_|"無法確認會議附件。")?;
                let b=std::fs::symlink_metadata(&output).map_err(|_|"無法確認會議附件。")?;
                if !a.is_file() || !b.is_file() || a.dev()!=b.dev() || a.ino()!=b.ino(){return Err("會議附件已變更。".into());}
            }
            std::fs::remove_file(staged).map_err(|_|"無法完成上傳暫存清理。")?;
        }
        let source=pipeline::validate_attachment(&self.manager.agent_dir(&u.agent),&output,u.size)?;
        if std::fs::metadata(&source).map_err(|_|"無法讀取會議附件。")?.len()!=u.size{return Err("會議附件大小不符。".into());}
        check()?;
        let admission=self.manager.prepare_uploaded(&u.agent,&source.to_string_lossy(),id,binding)?;
        self.admit_bound(&u.agent,admission.binding()).await?;
        check()?;
        let job=admission.commit_checked(check)?;
        audit.outcome="allowed";
        Ok(job.public())
    }
    pub async fn cancel_checked_for(&self,owner:&str,id:&str,check:impl Fn()->Result<(),String>)->Result<Value,String>{
        let _gate=self.gate.lock().await;
        let u=self.get_for(owner,id)?;
        let mut audit=MutationAudit{home:&self.manager.home,event:"upload_cancel",owner,agent:&u.agent,id:id.into(),outcome:"cancellation_refused"};
        check()?;
        if u.job_id.is_some(){return Err("上傳已建立轉錄工作，請取消該工作。".into());}
        self.store.expire(id)?;
        audit.outcome="allowed";
        // Reservation is released even if a later filesystem cleanup must retry.
        for path in [Some(self.staged(id)?),if u.state=="completing"{Some(self.output(&u)?)}else{None}].into_iter().flatten(){
            match std::fs::remove_file(path){Ok(())=>(),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>(),Err(_)=>return Err("上傳已取消，但暫存清理失敗。".into())}
        }
        self.store.cleaned(id)?;
        Ok(json!({"upload_id":id,"state":"cancelled"}))
    }
    fn sweep(&self)->Result<(),String>{
        for u in self.store.expired()?{
            self.store.expire(&u.id)?;
            let path=self.staged(&u.id)?;
            if path.exists(){std::fs::remove_file(path).map_err(|_|"無法清理過期上傳。")?;}
            if !u.extension.is_empty() {
                let path=self.output(&u)?;
                if path.exists(){std::fs::remove_file(path).map_err(|_|"無法清理過期上傳。")?;}
            }
            self.store.cleaned(&u.id)?;
        }Ok(())
    }
    async fn admit(&self,agent:&str)->Result<(),String>{
        self.admit_bound(agent,json!({"source":"meeting_upload"})).await
    }
    async fn admit_bound(&self,agent:&str,args:Value)->Result<(),String>{
        let policy=self.manager.policy_digest(agent)?;
        use crate::computer_use_sessions::gates;
        let home=&self.manager.home;
        self.manager.employee(agent)?;
        self.manager.background_allowed(agent)?;
        gates::tool_gates(home,agent,"meeting_transcribe").await.map_err(|e|e.message)?;
        if !duduclaw_core::agent_toml::load(&self.manager.agent_dir(agent)).capabilities.policy.is_empty(){
            return Err("這位員工設有轉錄參數限制，請由管理員確認權限設定後再上傳。".into());
        }
        if gates::approval_required(home,agent,"meeting_transcribe") {
            return Err("這位員工的轉錄需要額外核准，請由管理員確認權限設定後再上傳。".into());
        }
        let review=gates::action_review(home,agent,"meeting_transcribe",&args).await.map_err(|e|e.message)?;
        let call_id=format!("{}:{}:{}",uuid::Uuid::new_v4(),args["source_sha256"].as_str().unwrap_or("pending"),args["source_bytes"].as_u64().unwrap_or(0));
        let egress=gates::egress_check_with_call(home,agent,"meeting_transcribe",None,&call_id).map_err(|e|e.message)?;
        if self.manager.policy_digest(agent)?!=policy {return Err("員工權限已變更，請重新上傳。".into());}
        if review || egress.is_some(){return Err("這位員工的轉錄需要額外核准，請由管理員確認權限設定後再上傳。".into());}
        Ok(())
    }
}

fn sniff(bytes:&[u8])->Option<&'static str>{
    if bytes.get(..4)==Some(b"RIFF") && bytes.get(8..12)==Some(b"WAVE"){Some("wav")}
    else if bytes.get(4..8)==Some(b"ftyp"){Some("mp4")}
    else if bytes.get(..4)==Some(&[0x1a,0x45,0xdf,0xa3]){Some("webm")}
    else if bytes.get(..4)==Some(b"OggS"){Some("ogg")}
    else if bytes.get(..4)==Some(b"fLaC"){Some("flac")}
    else if bytes.get(..4)==Some(b"FORM") && matches!(bytes.get(8..12),Some(b"AIFF"|b"AIFC")){Some("aiff")}
    else if bytes.get(..3)==Some(b"ID3"){Some("mp3")}
    else if bytes.len()>=2 && bytes[0]==0xff && bytes[1]&0xf6==0xf0{Some("aac")}
    else if bytes.len()>=2 && bytes[0]==0xff && bytes[1]&0xe0==0xe0{Some("mp3")}
    else{None}
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn content_sniff_does_not_trust_a_client_extension(){
        assert_eq!(sniff(b"<script>fake.mp4</script>"),None);
        assert_eq!(sniff(b"RIFF1234WAVEdata"),Some("wav"));
        assert_eq!(sniff(b"0000ftypmp42"),Some("mp4"));
        assert_eq!(sniff(b"ID3audio"),Some("mp3"));
        assert_eq!(sniff(&[]),None);
    }
}

#[cfg(test)]mod protocol_tests{
    use super::*;
    fn setup()->(tempfile::TempDir,Arc<crate::meeting_jobs::Manager>){
        let home=tempfile::tempdir().unwrap();
        let path=home.path().canonicalize().unwrap();
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
        let manager=crate::meeting_jobs::Manager::open(path).unwrap();
        (home,manager)
    }
    #[tokio::test]
    async fn partial_upload_resumes_hashes_and_completes_once_without_provider_calls(){
        let (_home,manager)=setup();
        let service=Service::open(manager.clone()).unwrap();
        let size=CHUNK_SIZE+12;
        let created=service.create_for("person","alice",size).await.unwrap();
        let id=created["upload_id"].as_str().unwrap();
        let mut first=vec![0;CHUNK_SIZE as usize];first[..12].copy_from_slice(b"RIFF0000WAVE");
        let hash=hex::encode(Sha256::digest(&first));
        assert!(service.put_for("person",id,0,&"0".repeat(64),Bytes::from(first.clone())).await.is_err());
        assert_eq!(service.status_for("person",id).unwrap()["received"],json!([]));
        service.put_for("person",id,0,&hash,Bytes::from(first)).await.unwrap();
        assert!(service.complete_for("person",id,None).await.is_err());
        assert_eq!(service.get_for("someone-else",id).err(),service.get_for("person","00000000-0000-0000-0000-000000000000").err());
        let resumed=Service::open(manager.clone()).unwrap();
        let receipt=resumed.status_for("person",id).unwrap();
        assert_eq!(receipt["received"],json!([0]));assert_eq!(receipt["hashes"]["0"],hash);
        let last=Bytes::from_static(b"abcdefghijkl");
        let last_hash=hex::encode(Sha256::digest(&last));
        assert!(resumed.put_for("person",id,1,&last_hash,Bytes::from_static(b"short")).await.is_err());
        resumed.put_for("person",id,1,&last_hash,last).await.unwrap();
        let job=resumed.complete_for("person",id,None).await.unwrap();
        assert_eq!(job["state"],"queued");
        assert_eq!(resumed.complete_for("person",id,None).await.unwrap()["job_id"],job["job_id"]);
        assert_eq!(manager.list("alice").unwrap().len(),1);
        let persisted=manager.get("alice",job["job_id"].as_str().unwrap()).unwrap();
        assert_eq!(persisted.source_upload_id.as_deref(),Some(id));
        assert!(!std::path::Path::new(&persisted.source).exists(),"imported source is removed after snapshot admission");
        assert_eq!(std::fs::metadata(&persisted.snapshot_path).unwrap().len(),size);
        pipeline::verify_snapshot(std::path::Path::new(&persisted.snapshot_path),&persisted.source_sha256,size).unwrap();
    }
    #[tokio::test]
    async fn size_shared_quota_and_expiry_refuse_without_accepting_more_bytes(){
        let (_home,manager)=setup();
        std::fs::write(manager.home.join("config.toml"),format!("[security]\negress_guard='off'\n[transcription]\nmax_bytes=64\nstaging_quota_bytes={}\n",2*(store::CHUNK_RESERVE+64))).unwrap();
        let service=Service::open(manager.clone()).unwrap();
        assert!(service.create_for("person","alice",0).await.is_err());
        assert!(service.create_for("person","alice",65).await.is_err());
        let value=service.create_for("person","alice",64).await.unwrap();
        let id=value["upload_id"].as_str().unwrap();
        assert!(service.create_for("person","alice",1).await.is_err());
        std::fs::write(manager.home.join("agents/alice/attachments/ordinary.wav"),b"a").unwrap();
        assert!(manager.create("alice","ordinary.wav",None,"zh-TW",None).is_err(),"ordinary jobs share upload quota");
        let db=rusqlite::Connection::open(manager.home.join("meeting_jobs.db")).unwrap();
        db.execute("UPDATE meeting_uploads SET expires=0 WHERE id=?1",[id]).unwrap();
        assert!(service.get_for("person",id).is_err());
        service.sweep().unwrap();
        assert!(!service.staged(id).unwrap().exists());
        assert!(service.create_for("person","alice",1).await.is_ok());
    }
}
