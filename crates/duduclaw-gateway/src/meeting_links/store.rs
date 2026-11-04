//! Single-upload capability. Only a SHA-256 digest of its 256-bit secret is stored.
use std::{path::Path,sync::Mutex};
use rusqlite::{Connection,OptionalExtension,params};
use serde::{Serialize,Deserialize};
use sha2::{Digest,Sha256};
use crate::meeting_jobs::NotificationBinding;
pub const INVALID:&str="這個上傳連結已失效或已使用，請在原對話索取新連結。";
#[derive(Clone,Serialize,Deserialize)]
pub struct Link {
    pub id:String,
    pub agent:String,
    pub binding:NotificationBinding,
    pub max_bytes:u64,
    pub policy_digest:String,
    pub expires:i64,
    pub upload_id:Option<String>,
    pub state:String,
}
pub struct Store(Mutex<Connection>);
impl Store {
    pub fn open(home:&Path)->Result<Self,String>{
        let c=Connection::open(home.join("meeting_jobs.db")).map_err(|_|INVALID)?;
        c.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_|INVALID)?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS meeting_upload_links(hash TEXT PRIMARY KEY,id TEXT UNIQUE NOT NULL,state TEXT NOT NULL,data TEXT NOT NULL)").map_err(|_|INVALID)?;
        Ok(Self(Mutex::new(c)))
    }
    pub fn issue(&self,agent:&str,binding:NotificationBinding,max_bytes:u64,policy_digest:String,now:i64)->Result<String,String>{
        let secret:[u8;32]=rand::random();
        let token=hex::encode(secret);let hash=digest(&token);
        let link=Link{id:uuid::Uuid::new_v4().to_string(),agent:agent.into(),binding,max_bytes,policy_digest,expires:now+1800,upload_id:None,state:"fresh".into()};
        self.0.lock().map_err(|_|INVALID)?.execute("INSERT INTO meeting_upload_links VALUES (?1,?2,?3,?4)",params![hash,link.id,link.state,serde_json::to_string(&link).map_err(|_|INVALID)?]).map_err(|_|INVALID)?;
        Ok(token)
    }
    pub fn get(&self,token:&str,now:i64)->Result<Link,String>{
        if token.len()!=64 || !token.bytes().all(|b|b.is_ascii_hexdigit()){return Err(INVALID.into());}
        let hash=digest(token);
        let row:Option<(String,String)>=self.0.lock().map_err(|_|INVALID)?.query_row("SELECT hash,data FROM meeting_upload_links WHERE hash=?1",[&hash],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|INVALID)?;
        let (stored,data)=row.unwrap_or_else(||("0".repeat(64),String::new()));
        if !crate::auth::constant_time_eq(hash.as_bytes(),stored.as_bytes()) {return Err(INVALID.into());}
        let link:Link=serde_json::from_str(&data).map_err(|_|INVALID)?;
        if now>=link.expires || matches!(link.state.as_str(),"done"|"revoked") {return Err(INVALID.into());}
        Ok(link)
    }
    pub fn change(&self,id:&str,from:&str,to:&str,upload:Option<&str>)->Result<Link,String>{
        let mut c=self.0.lock().map_err(|_|INVALID)?;
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_|INVALID)?;
        let data:Option<String>=tx.query_row("SELECT data FROM meeting_upload_links WHERE id=?1 AND state=?2",params![id,from],|r|r.get(0)).optional().map_err(|_|INVALID)?;
        let mut link:Link=serde_json::from_str(&data.ok_or(INVALID)?).map_err(|_|INVALID)?;
        link.state=to.into(); if let Some(upload)=upload{link.upload_id=Some(upload.into());}
        // The link must be redeemed within 30 minutes. An admitted upload can
        // resume for 24 hours, but can never create a second upload.
        if to=="uploading" {link.expires=chrono::Utc::now().timestamp()+24*3600;}
        tx.execute("UPDATE meeting_upload_links SET state=?1,data=?2 WHERE id=?3",params![to,serde_json::to_string(&link).map_err(|_|INVALID)?,id]).map_err(|_|INVALID)?;
        tx.commit().map_err(|_|INVALID)?;Ok(link)
    }
}
fn digest(token:&str)->String{hex::encode(Sha256::digest(token.as_bytes()))}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_is_hashed_expiring_single_use_and_bound(){
        let h=tempfile::tempdir().unwrap();let s=Store::open(h.path()).unwrap();
        let binding=NotificationBinding{session:Some("telegram:42".into()),requester:Some("person".into()),..Default::default()};
        let token=s.issue("alice",binding,123,"policy".into(),100).unwrap();
        let j=s.get(&token,101).unwrap();assert_eq!(j.agent,"alice");assert_eq!(j.binding.session.as_deref(),Some("telegram:42"));
        assert!(s.get(&token,1900).is_err());assert!(s.get(&"a".repeat(64),101).is_err());
        s.change(&j.id,"fresh","creating",None).unwrap();assert!(s.change(&j.id,"fresh","creating",None).is_err());
        s.change(&j.id,"creating","uploading",Some("upload")).unwrap();
        assert_eq!(s.get(&token,102).unwrap().upload_id.as_deref(),Some("upload"));
        s.change(&j.id,"uploading","done",None).unwrap();assert!(s.get(&token,102).is_err());
        let c=s.0.lock().unwrap();let row:String=c.query_row("SELECT data FROM meeting_upload_links",[],|r|r.get(0)).unwrap();assert!(!row.contains(&token));
    }
}
