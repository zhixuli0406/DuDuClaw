//! Bounded body reads and one in-flight request per authenticated principal.
use std::{collections::HashSet, sync::{Mutex,OnceLock}, time::Duration};
use axum::body::{Body,Bytes};
use futures_util::StreamExt;
pub(crate) static UPLOAD_SLOTS:tokio::sync::Semaphore=tokio::sync::Semaphore::const_new(4);
pub(crate) static DOWNLOAD_SLOTS:tokio::sync::Semaphore=tokio::sync::Semaphore::const_new(4);
static OWNERS:OnceLock<Mutex<HashSet<String>>>=OnceLock::new();
pub(crate) struct Flight(String);
impl Flight {
    pub(crate) fn acquire(owner:&str)->Result<Self,String>{
        let mut owners=OWNERS.get_or_init(Default::default).lock().map_err(|_|"上傳服務忙碌，請稍後重試。")?;
        if owners.len()>=128 || !owners.insert(owner.to_owned()){return Err("同一上傳者已有請求正在處理，請稍後重試。".into());}
        Ok(Self(owner.to_owned()))
    }
}
impl Drop for Flight {fn drop(&mut self){if let Some(owners)=OWNERS.get(){if let Ok(mut owners)=owners.lock(){owners.remove(&self.0);}}}}
/// Every 30-second window must deliver at least 32 KiB/s; stalled streams
/// cannot reset the clock by trickling individual frames.
pub(crate) async fn read_body(body:Body,limit:usize)->Result<Bytes,String>{
    read_body_window(body,limit,Duration::from_secs(30),32*1024*30).await
}
async fn read_body_window(body:Body,limit:usize,window:Duration,min_bytes:usize)->Result<Bytes,String>{
    let mut stream=body.into_data_stream();let mut bytes=Vec::new();
    let mut deadline=tokio::time::Instant::now()+window;let mut window_bytes=0usize;
    loop {
        if tokio::time::Instant::now()>=deadline {
            if window_bytes<min_bytes{return Err("上傳速度過慢或閒置逾時，請重試。".into());}
            deadline=tokio::time::Instant::now()+window;window_bytes=0;
        }
        match tokio::time::timeout_at(deadline,stream.next()).await {
            Ok(Some(Ok(frame)))=>{
                if frame.len()>limit.saturating_sub(bytes.len()){return Err("上傳分段過大。".into());}
                window_bytes=window_bytes.saturating_add(frame.len());bytes.extend_from_slice(&frame);
            },
            Ok(Some(Err(_)))=>return Err("上傳分段不完整。".into()),
            Ok(None)=>return Ok(Bytes::from(bytes)),
            Err(_)=>{
                if window_bytes<min_bytes{return Err("上傳速度過慢或閒置逾時，請重試。".into());}
                deadline=tokio::time::Instant::now()+window;window_bytes=0;
            }
        }
    }
}
#[cfg(test)]mod tests {
    use super::*;
    #[tokio::test]async fn slow_stream_and_size_limit_are_enforced(){
        let body=Body::from_stream(futures_util::stream::pending::<Result<Bytes,std::io::Error>>());
        assert!(read_body_window(body,1024,Duration::from_millis(10),1).await.is_err());
        assert!(read_body(Body::from("large"),2).await.is_err());
        assert_eq!(read_body(Body::from("ok"),2).await.unwrap(),"ok");
    }
    #[test]fn owner_flight_releases_on_drop(){
        let first=Flight::acquire("test-owner-flight").unwrap();
        assert!(Flight::acquire("test-owner-flight").is_err());
        assert!(Flight::acquire("other-owner-flight").is_ok());
        drop(first);assert!(Flight::acquire("test-owner-flight").is_ok());
    }
}
