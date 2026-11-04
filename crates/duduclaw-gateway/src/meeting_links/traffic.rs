//! Admission precedes token hashing and SQLite, including valid capabilities.
use std::{collections::HashMap,sync::Mutex,time::Instant};
struct Bucket {tokens:f64,at:Instant}
impl Bucket {
    fn take(&mut self,now:Instant,capacity:f64,rate:f64)->bool{
        self.tokens=(self.tokens+now.duration_since(self.at).as_secs_f64()*rate).min(capacity);self.at=now;
        if self.tokens<1.0{return false;}self.tokens-=1.0;true
    }
}
#[derive(Default)]pub(super) struct Admission {buckets:Mutex<HashMap<String,Bucket>>}
impl Admission {
    fn allow(&self,key:String,now:Instant,capacity:f64,rate:f64)->bool{
        let Ok(mut buckets)=self.buckets.lock()else{return false;};
        buckets.retain(|_,b|now.duration_since(b.at).as_secs()<120);
        if !buckets.contains_key(&key) && buckets.len()>=4096{return false;}
        buckets.entry(key).or_insert(Bucket{tokens:capacity,at:now}).take(now,capacity,rate)
    }
    pub(super) fn pre_auth(&self,ip:std::net::IpAddr,now:Instant)->bool{
        self.allow(format!("ip:{ip}"),now,30.0,1.0) && self.allow("global".into(),now,120.0,4.0)
    }
    pub(super) fn link(&self,id:&str,now:Instant)->bool{self.allow(format!("link:{id}"),now,60.0,2.0)}
}
pub(super) static AUTH_SLOTS:tokio::sync::Semaphore=tokio::sync::Semaphore::const_new(8);
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn global_ip_and_link_limits_are_separate(){
        let a=Admission::default();let now=Instant::now();let ip="127.0.0.1".parse().unwrap();
        for _ in 0..30{assert!(a.pre_auth(ip,now));}
        for _ in 0..200{assert!(!a.pre_auth(ip,now));}assert!(a.pre_auth("127.0.0.2".parse().unwrap(),now));
        for _ in 0..60{assert!(a.link("one",now));}assert!(!a.link("one",now));assert!(a.link("two",now));
        assert!(a.pre_auth(ip,now+std::time::Duration::from_secs(1)));
    }
}
