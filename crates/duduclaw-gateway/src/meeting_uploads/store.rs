use std::{path::Path, sync::Mutex};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value,json};

pub const CHUNK_SIZE:u64=8*1024*1024;
pub const CHUNK_RESERVE:u64=64*1024;
const IDLE_SECONDS:i64=2*3600;
pub const NOT_FOUND:&str="找不到這次上傳，可能已過期。";
const IO:&str="無法讀取上傳進度，請稍後再試。";
#[derive(Clone)]
pub struct Upload {pub id:String,pub agent:String,pub owner:String,pub size:u64,pub expires:i64,pub state:String,pub extension:String,pub job_id:Option<String>}
impl Upload {
    pub fn chunks(&self)->u64 {self.size.div_ceil(CHUNK_SIZE)}
    pub fn expected(&self,index:u64)->Result<usize,String> {
        if index>=self.chunks(){return Err("上傳分段編號無效。".into());}
        Ok((self.size-index*CHUNK_SIZE).min(CHUNK_SIZE) as usize)
    }
    pub fn receipt(&self,received:Vec<u64>,max_bytes:u64)->Value {
        json!({"upload_id":self.id,"chunk_size":CHUNK_SIZE,"received":received,"max_bytes":max_bytes,"expires_at":self.expires,"job_id":self.job_id})
    }
}
pub struct Store(Mutex<Connection>);
impl Store {
    pub fn open(home:&Path)->Result<Self,String>{
        let c=Connection::open(home.join("meeting_jobs.db")).map_err(|_|IO)?;
        c.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_|IO)?;
        c.execute_batch("CREATE INDEX IF NOT EXISTS meeting_upload_owner_active ON meeting_uploads(owner,state,expires); CREATE INDEX IF NOT EXISTS meeting_upload_agent_active ON meeting_uploads(agent,state,expires);").map_err(|_|IO)?;
        Ok(Self(Mutex::new(c)))
    }
    pub fn create(&self,agent:&str,owner:&str,size:u64,quota:u64)->Result<Upload,String>{
        let mut c=self.0.lock().map_err(|_|IO)?;
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_|IO)?;
        let now=chrono::Utc::now().timestamp();
        // Expiration releases reservations in this same admission transaction.
        tx.execute("UPDATE meeting_uploads SET reserved=0 WHERE expires<=?1 AND job_id IS NULL",[now]).map_err(|_|IO)?;
        let reserved=size.checked_add(CHUNK_RESERVE).ok_or("檔案過大。")?;
        for (column,value) in [("owner",owner),("agent",agent)] {
            let (count,bytes):(u64,u64)=tx.query_row(&format!("SELECT COUNT(*),COALESCE(SUM(reserved),0) FROM meeting_uploads WHERE {column}=?1 AND expires>?2 AND state IN ('uploading','completing')"),params![value,now],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|IO)?;
            if count>=2 || reserved> (quota/2).saturating_sub(bytes) {return Err("同一上傳者或員工的待處理上傳已達上限，請先完成或取消上傳。".into());}
        }
        let used:u64=tx.query_row("SELECT (SELECT COALESCE(SUM(reserved),0) FROM meeting_jobs)+(SELECT COALESCE(SUM(reserved),0) FROM meeting_uploads)",[],|r|r.get(0)).map_err(|_|IO)?;
        if reserved>quota.saturating_sub(used){return Err("會議暫存空間已滿，請等待其他轉錄完成後再試。".into());}
        let u=Upload{id:uuid::Uuid::new_v4().to_string(),agent:agent.into(),owner:owner.into(),size,expires:now+IDLE_SECONDS,state:"uploading".into(),extension:String::new(),job_id:None};
        tx.execute("INSERT INTO meeting_uploads(id,agent,owner,size,reserved,expires,state,extension) VALUES (?1,?2,?3,?4,?5,?6,'uploading','')",params![u.id,u.agent,u.owner,size,reserved,u.expires]).map_err(|_|IO)?;
        tx.execute("INSERT INTO meeting_job_events(job_id,agent,event,at) VALUES (?1,?2,'upload_create',?3)",params![u.id,u.agent,chrono::Utc::now().timestamp()]).map_err(|_|IO)?;
        tx.commit().map_err(|_|IO)?;Ok(u)
    }
    pub fn get(&self,id:&str,owner:&str)->Result<Upload,String>{
        if uuid::Uuid::parse_str(id).is_err(){return Err(NOT_FOUND.into());}
        let c=self.0.lock().map_err(|_|IO)?;
        c.query_row("SELECT id,agent,owner,size,expires,state,extension,job_id FROM meeting_uploads WHERE id=?1 AND owner=?2 AND expires>?3 AND state IN ('uploading','completing','done')",params![id,owner,chrono::Utc::now().timestamp()],|r|Ok(Upload{id:r.get(0)?,agent:r.get(1)?,owner:r.get(2)?,size:r.get(3)?,expires:r.get(4)?,state:r.get(5)?,extension:r.get(6)?,job_id:r.get(7)?})).optional().map_err(|_|IO)?.ok_or_else(||NOT_FOUND.into())
    }
    pub fn received(&self,id:&str)->Result<Vec<u64>,String>{
        let c=self.0.lock().map_err(|_|IO)?;
        let mut q=c.prepare("SELECT chunk_index FROM meeting_upload_chunks WHERE upload_id=?1 ORDER BY chunk_index").map_err(|_|IO)?;
        let rows=q.query_map([id],|r|r.get::<_,u64>(0)).map_err(|_|IO)?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|_|IO.into())
    }
    pub fn hash(&self,id:&str,index:u64)->Result<Option<String>,String>{
        let c=self.0.lock().map_err(|_|IO)?;
        c.query_row("SELECT sha256 FROM meeting_upload_chunks WHERE upload_id=?1 AND chunk_index=?2",params![id,index],|r|r.get(0)).optional().map_err(|_|IO.into())
    }
    pub fn hashes(&self,id:&str)->Result<Value,String>{
        let c=self.0.lock().map_err(|_|IO)?;
        let mut q=c.prepare("SELECT chunk_index,sha256 FROM meeting_upload_chunks WHERE upload_id=?1 ORDER BY chunk_index").map_err(|_|IO)?;
        let rows=q.query_map([id],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?))).map_err(|_|IO)?;
        let mut hashes=serde_json::Map::new();
        for row in rows{let (i,h)=row.map_err(|_|IO)?;hashes.insert(i.to_string(),h.into());}
        Ok(Value::Object(hashes))
    }
    pub fn record_chunk(&self,id:&str,index:u64,hash:&str,extension:Option<&str>)->Result<(),String>{
        let mut c=self.0.lock().map_err(|_|IO)?;
        let tx=c.transaction().map_err(|_|IO)?;
        tx.execute("INSERT OR REPLACE INTO meeting_upload_chunks VALUES (?1,?2,?3)",params![id,index,hash]).map_err(|_|IO)?;
        if let Some(ext)=extension{tx.execute("UPDATE meeting_uploads SET extension=?2 WHERE id=?1",params![id,ext]).map_err(|_|IO)?;}
        tx.execute("UPDATE meeting_uploads SET expires=?2 WHERE id=?1 AND state='uploading'",params![id,chrono::Utc::now().timestamp()+IDLE_SECONDS]).map_err(|_|IO)?;
        tx.commit().map_err(|_|IO.into())
    }
    pub fn completing(&self,id:&str)->Result<(),String>{
        let c=self.0.lock().map_err(|_|IO)?;
        if c.execute("UPDATE meeting_uploads SET state='completing' WHERE id=?1 AND state IN ('uploading','completing')",[id]).map_err(|_|IO)?!=1{return Err("上傳狀態已變更。".into());}Ok(())
    }
    pub fn budget(&self,quota:u64)->Result<(),String>{
        let c=self.0.lock().map_err(|_|IO)?;
        let used:u64=c.query_row("SELECT (SELECT COALESCE(SUM(reserved),0) FROM meeting_jobs)+(SELECT COALESCE(SUM(reserved),0) FROM meeting_uploads)",[],|r|r.get(0)).map_err(|_|IO)?;
        if used>quota{return Err("會議暫存配額已調整，請稍後再試。".into());}Ok(())
    }
    pub fn expired(&self)->Result<Vec<Upload>,String>{
        let c=self.0.lock().map_err(|_|IO)?;
        let mut q=c.prepare("SELECT id,agent,owner,size,expires,state,extension,job_id FROM meeting_uploads WHERE expires<=?1 AND state NOT IN ('done','expired_cleaned')").map_err(|_|IO)?;
        let rows=q.query_map([chrono::Utc::now().timestamp()],|r|Ok(Upload{id:r.get(0)?,agent:r.get(1)?,owner:r.get(2)?,size:r.get(3)?,expires:r.get(4)?,state:r.get(5)?,extension:r.get(6)?,job_id:r.get(7)?})).map_err(|_|IO)?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|_|IO.into())
    }
    pub fn cleaned(&self,id:&str)->Result<(),String>{
        self.0.lock().map_err(|_|IO)?.execute("UPDATE meeting_uploads SET state='expired_cleaned' WHERE id=?1 AND state='expired'",[id]).map_err(|_|IO)?;Ok(())
    }
    pub fn expire(&self,id:&str)->Result<(),String>{
        let mut c=self.0.lock().map_err(|_|IO)?;
        let tx=c.transaction().map_err(|_|IO)?;
        tx.execute("UPDATE meeting_uploads SET state='expired',reserved=0,expires=0 WHERE id=?1 AND job_id IS NULL",[id]).map_err(|_|IO)?;
        tx.execute("DELETE FROM meeting_upload_chunks WHERE upload_id=?1",[id]).map_err(|_|IO)?;
        tx.commit().map_err(|_|IO.into())
    }
}

#[cfg(test)]mod tests{
    use super::*;
    fn setup()->(tempfile::TempDir,Store){
        let home=tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        let _manager=crate::meeting_jobs::Manager::open(home.path().to_path_buf()).unwrap();
        let store=Store::open(home.path()).unwrap();(home,store)
    }
    #[test]fn owners_and_employees_have_independent_count_and_byte_limits(){
        let (_h,s)=setup();let quota=16*CHUNK_RESERVE;
        let first=s.create("alice","one",1,quota).unwrap();
        s.create("bob","one",1,quota).unwrap();
        assert!(s.create("carol","one",1,quota).is_err(),"one owner cannot hoard rows across employees");
        s.create("alice","two",1,quota).unwrap();
        assert!(s.create("alice","three",1,quota).is_err(),"one employee cannot hoard rows across owners");
        assert!(s.create("carol","three",1,quota).is_ok(),"other owners retain capacity");
        s.expire(&first.id).unwrap();
        assert!(s.create("dave","one",1,quota).is_ok(),"cancel immediately releases count and bytes");
        assert!(s.create("large","large",quota/2,quota).is_err(),"principal bytes are bounded including overhead");
    }
    #[test]fn expired_rows_release_quota_inside_admission_transaction(){
        let (_h,s)=setup();let quota=4*CHUNK_RESERVE;
        let old=s.create("alice","one",1,quota).unwrap();
        s.0.lock().unwrap().execute("UPDATE meeting_uploads SET expires=0 WHERE id=?1",[&old.id]).unwrap();
        assert!(s.create("alice","one",1,quota).is_ok());
        let reserved:u64=s.0.lock().unwrap().query_row("SELECT reserved FROM meeting_uploads WHERE id=?1",[&old.id],|r|r.get(0)).unwrap();
        assert_eq!(reserved,0);
    }
    #[test]fn idle_expiry_refreshes_only_after_a_verified_chunk(){
        let (_h,s)=setup();let u=s.create("alice","one",1,1024*1024).unwrap();
        assert!(u.expires<=chrono::Utc::now().timestamp()+2*3600);
        s.0.lock().unwrap().execute("UPDATE meeting_uploads SET expires=?2 WHERE id=?1",params![u.id,chrono::Utc::now().timestamp()+10]).unwrap();
        s.record_chunk(&u.id,0,"hash",None).unwrap();
        assert!(s.get(&u.id,"one").unwrap().expires>chrono::Utc::now().timestamp()+3600);
    }
}
