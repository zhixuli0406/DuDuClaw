//! User-selected Drive media, streamed through the ordinary upload ledger.
use super::*;
use crate::google_personal::meeting_access::MeetingAccess;
use duduclaw_auth::UserDb;
use serde::{Deserialize,Serialize};
use std::{path::Path,time::Duration};

const DRIVE_FILES:&str="https://www.googleapis.com/drive/v3/files";
const MEET_RECORDS:&str="https://meet.googleapis.com/v2/conferenceRecords";
const MAX_JSON:usize=1024*1024;
const METADATA_FIELDS:&str="id,name,mimeType,size,modifiedTime,trashed,isAppAuthorized,capabilities(canDownload),version";
const UNAVAILABLE:&str="無法讀取 Google 錄影，請確認檔案已授權、仍可下載且連線有效。";

pub(super) fn valid_file_id(id:&str)->bool{
    (10..=200).contains(&id.len()) && id.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'))
}
fn valid_resource_id(id:&str)->bool{
    !id.is_empty() && id.len()<=200 && id.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'))
}
fn media_mime(mime:&str)->bool{
    matches!(mime.split_once('/'),Some(("audio"|"video",sub)) if !sub.is_empty() && sub.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'.'|b'+'|b'-')))
}
fn page_token(token:&str)->Result<(),String>{
    if token.len()>2048 || token.chars().any(char::is_control){return Err("Google 分頁資訊無效，請重新整理。".into());}Ok(())
}
fn next_page(value:&Value)->Result<Option<String>,String>{
    match value.get("nextPageToken"){
        None=>Ok(None),Some(Value::String(s))=>{page_token(s)?;Ok((!s.is_empty()).then(||s.clone()))},_=>Err(UNAVAILABLE.into())
    }
}
async fn json_response(mut response:reqwest::Response,check:impl Fn()->Result<(),String>)->Result<Value,String>{
    if response.status()!=reqwest::StatusCode::OK || response.content_length().is_some_and(|n|n>MAX_JSON as u64){return Err(UNAVAILABLE.into());}
    let deadline=tokio::time::Instant::now()+Duration::from_secs(30);
    let mut body=Vec::new();
    loop{
        check()?;
        let part=tokio::time::timeout_at(deadline,response.chunk()).await.map_err(|_|UNAVAILABLE)?.map_err(|_|UNAVAILABLE)?;
        check()?;
        let Some(part)=part else{break};
        if part.len()>MAX_JSON.saturating_sub(body.len()){return Err(UNAVAILABLE.into());}
        body.extend_from_slice(&part);
    }
    serde_json::from_slice(&body).map_err(|_|UNAVAILABLE.into())
}

#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct DriveFile{
    id:String,name:String,mime_type:String,size:Option<String>,modified_time:Option<String>,
    #[serde(default)]trashed:bool,
    #[serde(default)]is_app_authorized:bool,
    capabilities:Option<DownloadCapability>,version:Option<String>,
}
#[derive(Deserialize)]#[serde(rename_all="camelCase")]
struct DownloadCapability{can_download:Option<bool>}
#[derive(Serialize)]
pub(super) struct MediaFile{pub id:String,pub name:String,pub mime_type:String,pub size:u64,pub modified_time:Option<String>}
impl DriveFile{
    fn media(&self,max_bytes:u64)->Result<MediaFile,String>{
        let size=self.size.as_deref().and_then(|s|s.parse::<u64>().ok()).filter(|n|*n>0 && *n<=max_bytes).ok_or("錄影大小超過設定上限或無法確認，請選擇其他檔案。")?;
        if !valid_file_id(&self.id) || !media_mime(&self.mime_type) || self.trashed
            || !self.is_app_authorized || self.capabilities.as_ref().and_then(|c|c.can_download)!=Some(true)
            || self.name.is_empty() || self.name.len()>1024 || self.name.chars().any(char::is_control)
            || self.modified_time.as_ref().is_some_and(|s|s.len()>64 || s.chars().any(char::is_control)){
            return Err(UNAVAILABLE.into());
        }
        Ok(MediaFile{id:self.id.clone(),name:self.name.clone(),mime_type:self.mime_type.clone(),size,modified_time:self.modified_time.clone()})
    }
    fn version(&self)->Result<&str,String>{
        self.version.as_deref().filter(|v|!v.is_empty() && v.len()<=32 && v.bytes().all(|b|b.is_ascii_digit())).ok_or_else(||UNAVAILABLE.into())
    }
}
async fn metadata(home:&Path,db:&UserDb,access:&MeetingAccess,id:&str)->Result<DriveFile,String>{
    if !valid_file_id(id){return Err("Google 檔案代號無效。".into());}
    let mut url=reqwest::Url::parse(&format!("{DRIVE_FILES}/{id}")).map_err(|_|UNAVAILABLE)?;
    url.query_pairs_mut().append_pair("fields",METADATA_FIELDS).append_pair("supportsAllDrives","true");
    let response=access.get(home,db,url).await?;
    let file:DriveFile=serde_json::from_value(json_response(response,||access.validate(home,db)).await?).map_err(|_|UNAVAILABLE)?;
    if file.id!=id{return Err(UNAVAILABLE.into());}Ok(file)
}

pub(super) async fn files(home:&Path,db:&UserDb,access:&MeetingAccess,search:&str,token:&str,max_bytes:u64)->Result<Value,String>{
    page_token(token)?;
    if search.len()>256 || search.chars().any(char::is_control){return Err("搜尋文字過長或包含無效字元。".into());}
    let mut q="trashed = false and (mimeType contains 'audio/' or mimeType contains 'video/')".to_owned();
    if !search.is_empty(){q.push_str(&format!(" and name contains '{}'",search.replace('\\',"\\\\").replace('\'',"\\'")));}
    let mut url=reqwest::Url::parse(DRIVE_FILES).map_err(|_|UNAVAILABLE)?;
    url.query_pairs_mut().append_pair("q",&q).append_pair("pageSize","50").append_pair("orderBy","modifiedTime desc")
        .append_pair("fields",&format!("nextPageToken,files({METADATA_FIELDS})"));
    if !token.is_empty(){url.query_pairs_mut().append_pair("pageToken",token);}
    let response=access.get(home,db,url).await?;
    let value=json_response(response,||access.validate(home,db)).await?;
    let list=value.get("files").map(|v|serde_json::from_value::<Vec<DriveFile>>(v.clone())).transpose().map_err(|_|UNAVAILABLE)?.unwrap_or_default();
    if list.len()>50{return Err(UNAVAILABLE.into());}
    let files=list.iter().filter_map(|file|file.media(max_bytes).ok()).collect::<Vec<_>>();
    Ok(json!({"files":files,"next_page_token":next_page(&value)?}))
}

pub(super) async fn recordings(home:&Path,db:&UserDb,access:&MeetingAccess,token:&str)->Result<Value,String>{
    page_token(token)?;
    let mut url=reqwest::Url::parse(MEET_RECORDS).map_err(|_|UNAVAILABLE)?;
    url.query_pairs_mut().append_pair("pageSize","10");
    if !token.is_empty(){url.query_pairs_mut().append_pair("pageToken",token);}
    let response=access.get(home,db,url).await?;
    let value=json_response(response,||access.validate(home,db)).await?;
    let empty=Vec::new();
    let conferences=match value.get("conferenceRecords"){None=>&empty,Some(v)=>v.as_array().ok_or(UNAVAILABLE)?};
    if conferences.len()>10{return Err(UNAVAILABLE.into());}
    let mut recordings=Vec::new();let mut truncated=false;
    for conference in conferences{
        let name=conference["name"].as_str().ok_or(UNAVAILABLE)?;
        let id=name.strip_prefix("conferenceRecords/").filter(|id|valid_resource_id(id)).ok_or(UNAVAILABLE)?;
        let mut url=reqwest::Url::parse(&format!("{MEET_RECORDS}/{id}/recordings")).map_err(|_|UNAVAILABLE)?;
        url.query_pairs_mut().append_pair("pageSize","5");
        let response=access.get(home,db,url).await?;
        let value=json_response(response,||access.validate(home,db)).await?;
        truncated |= next_page(&value)?.is_some();
        let items=match value.get("recordings"){None=>&empty,Some(v)=>v.as_array().ok_or(UNAVAILABLE)?};
        if items.len()>5{return Err(UNAVAILABLE.into());}
        for item in items{
            if item["state"]!="FILE_GENERATED"{continue;}
            // Meet supplies a Drive file ID, never a URL to follow.
            let file_id=item["driveDestination"]["file"].as_str().filter(|id|valid_file_id(id)).ok_or(UNAVAILABLE)?;
            let start=item["startTime"].as_str().filter(|s|s.len()<=64 && chrono::DateTime::parse_from_rfc3339(s).is_ok()).ok_or(UNAVAILABLE)?;
            recordings.push(json!({"file_id":file_id,"start_time":start,"conference":name}));
        }
    }
    access.validate(home,db)?;
    Ok(json!({"recordings":recordings,"next_page_token":next_page(&value)?,"truncated":truncated}))
}

struct ImportGuard{service:Arc<Service>,owner:String,id:String,finished:bool}
impl Drop for ImportGuard{
    fn drop(&mut self){
        if self.finished{return;}
        let service=self.service.clone();let owner=self.owner.clone();let id=self.id.clone();
        // Cancellation/drop must release the reservation, too. Crash recovery
        // uses the upload ledger's ordinary idle expiry as a second line.
        tokio::spawn(async move{let _=service.cancel_checked_for(&owner,&id,||Ok(())).await;});
    }
}
impl Service{
    pub(super) async fn import_drive(self:&Arc<Self>,db:&UserDb,access:&MeetingAccess,agent:&str,file_id:&str,selection_receipt:Option<&str>,check:impl Fn()->Result<(),String>)->Result<Value,String>{
        let home=&self.manager.home;
        let mut audit=MutationAudit{home,event:"meeting_drive_import",owner:access.owner_id(),agent,id:file_id.into(),outcome:"import_refused"};
        if !valid_file_id(file_id){return Err("Google 檔案代號無效。".into());}
        check()?;access.validate(home,db)?;
        super::selection::consume(home,access.owner_id(),agent,file_id,selection_receipt)?;
        let policy=self.manager.policy_digest(agent)?;
        let live=||{
            check()?;access.validate(home,db)?;
            self.manager.employee(agent)?;self.manager.background_allowed(agent)?;
            if self.manager.policy_digest(agent)?!=policy{return Err("員工權限已變更，請重新匯入。".into());}
            Ok(())
        };
        live()?;self.admit(agent).await?;live()?;
        let file=metadata(home,db,access,file_id).await?;
        let media=file.media(TranscriptionConfig::load(home)?.max_bytes)?;
        let version=file.version()?.to_owned();
        live()?;
        let receipt=self.create_checked_for(access.owner_id(),agent,media.size,&live).await?;
        let id=receipt["upload_id"].as_str().ok_or(UNAVAILABLE)?.to_owned();
        let mut guard=ImportGuard{service:self.clone(),owner:access.owner_id().into(),id:id.clone(),finished:false};
        let outcome=async{
            self.store.record_drive_source(&id,access.owner_id(),access.account_email(),file_id,&version)?;
            let mut url=reqwest::Url::parse(&format!("{DRIVE_FILES}/{file_id}")).map_err(|_|UNAVAILABLE)?;
            url.query_pairs_mut().append_pair("alt","media").append_pair("supportsAllDrives","true");
            live()?;
            let response=access.get(home,db,url).await?;
            receive_media(self,access.owner_id(),&id,media.size,response,&live).await?;
            let after=metadata(home,db,access,file_id).await?;
            if after.version()?!=version || after.media(TranscriptionConfig::load(home)?.max_bytes)?.size!=media.size{return Err("Google 錄影在匯入期間已變更，請重新選擇。".into());}
            live()?;
            self.complete_checked_for(access.owner_id(),&id,None,&live).await
        }.await;
        match outcome{
            Ok(job)=>{guard.finished=true;audit.outcome="allowed";Ok(job)},
            Err(error)=>{let _=self.cancel_checked_for(access.owner_id(),&id,||Ok(())).await;Err(error)}
        }
    }
}

async fn receive_media(service:&Service,owner:&str,id:&str,size:u64,mut response:reqwest::Response,check:impl Fn()->Result<(),String>)->Result<(),String>{
    if response.status()!=reqwest::StatusCode::OK || response.content_length().is_some_and(|n|n!=size){return Err(UNAVAILABLE.into());}
    let deadline=tokio::time::Instant::now()+Duration::from_secs(30*60);
    let mut total=0u64;let mut index=0u64;let mut buffer=Vec::with_capacity(CHUNK_SIZE as usize);
    let mut window_end=tokio::time::Instant::now()+Duration::from_secs(30);let mut window_bytes=0u64;
    loop{
        check()?;
        let now=tokio::time::Instant::now();
        if now>=deadline{return Err("Google 錄影匯入逾時，請重試。".into());}
        if now>=window_end{
            if window_bytes<32*1024*30{return Err("Google 錄影下載速度過慢，請重試。".into());}
            window_end=now+Duration::from_secs(30);window_bytes=0;
        }
        let part=match tokio::time::timeout_at(deadline.min(window_end),response.chunk()).await{
            Ok(part)=>part.map_err(|_|UNAVAILABLE)?,
            Err(_)=>{
                check()?;
                if tokio::time::Instant::now()>=deadline || window_bytes<32*1024*30{return Err("Google 錄影下載逾時，請重試。".into());}
                window_end=tokio::time::Instant::now()+Duration::from_secs(30);window_bytes=0;continue;
            }
        };
        check()?;
        let Some(mut part)=part else{break};
        if part.len() as u64>size.saturating_sub(total){return Err("Google 錄影超過已確認的大小，匯入已停止。".into());}
        total+=part.len() as u64;window_bytes+=part.len() as u64;
        while !part.is_empty(){
            let count=(CHUNK_SIZE as usize-buffer.len()).min(part.len());
            buffer.extend_from_slice(&part.split_to(count));
            if buffer.len()==CHUNK_SIZE as usize{
                let hash=hex::encode(Sha256::digest(&buffer));
                service.put_checked_for(owner,id,index,&hash,Bytes::from(std::mem::take(&mut buffer)),&check).await?;
                index+=1;
            }
        }
    }
    if total!=size{return Err("Google 錄影下載不完整，請重試。".into());}
    if !buffer.is_empty(){
        let hash=hex::encode(Sha256::digest(&buffer));
        service.put_checked_for(owner,id,index,&hash,Bytes::from(buffer),&check).await?;
    }
    check()
}

#[cfg(test)]
mod tests;
