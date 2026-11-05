//! Finite mailbox around blocking host operations. The control loop never waits.
use std::{collections::BTreeMap, os::fd::OwnedFd, sync::mpsc::{self, SyncSender, Receiver}, time::{Duration, Instant}};
use duduclaw_isolation::{execution::{RunHandle, RuntimeKind, PtySize}, ids::{EmployeeId, RunId}, ipc::{CleanupReadback, ErrorCode, SpawnRequest}};
use crate::{admission::{Host, HostProgress, LaunchPolicy}, Result};
#[cfg(test)]
#[path = "host_worker_tests.rs"]
mod tests;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key { Ready(EmployeeId), Policy(EmployeeId, String, bool), Cleanup(EmployeeId, Option<RunId>), Start(RunId), Poll(RunId), Revoke(RunId), Kill(RunId), Io(RunId), Resize(RunId, u16, u16), Forget(RunId) }
enum Work { Ready(EmployeeId), Policy(EmployeeId, RuntimeKind, bool), Cleanup(EmployeeId, Option<RunHandle>), Start(RunHandle, EmployeeId, SpawnRequest, LaunchPolicy, Vec<OwnedFd>), Poll(RunHandle), Revoke(RunHandle), Kill(RunHandle), Io(RunHandle), Resize(RunHandle, PtySize), Forget(RunHandle) }
enum Value { Unit, Policy(LaunchPolicy), Cleanup(CleanupReadback), Progress(HostProgress), Io(Vec<OwnedFd>) }
struct Completed { key: Key, result: Result<Value>, at: Instant }
struct Slot { sent: Instant, result: Option<(Result<Value>, Instant)> }

/// The scanner has its own thread and deadline; no account/D-Bus command holds
/// its lock. A stale scan is a failure, even if the lifecycle worker is healthy.
pub trait Scan: Send {
    fn scan(&mut self) -> Result<Vec<EmployeeId>>;
    fn health(&mut self, employee: EmployeeId) -> Result<()>;
    fn register(&mut self, run: RunHandle, employee: EmployeeId, startup_ms: u64) -> Result<()>;
    fn cancel(&mut self, run: RunHandle) -> Result<()>;
}
pub struct WorkerHost {
    sender: SyncSender<(Key, Work)>, receiver: Receiver<Completed>, slots: BTreeMap<Key, Slot>,
    scanner: Box<dyn Scan>,
}
impl WorkerHost {
    pub fn spawn<H>(mut host: H, scanner: Box<dyn Scan>) -> std::io::Result<Self>
    where H: Host<Inputs = Vec<OwnedFd>, Io = Vec<OwnedFd>> + Send + 'static {
        let (sender, commands) = mpsc::sync_channel::<(Key, Work)>(128);
        let (completed, receiver) = mpsc::sync_channel(128);
        std::thread::Builder::new().name("spawnd-lifecycle".into()).spawn(move || {
            loop {
                let _ = host.maintenance();
                match commands.recv_timeout(Duration::from_millis(25)) {
                    Ok((key, work)) => {
                        let result = match work {
                            Work::Ready(e) => host.ready(e).map(|()| Value::Unit),
                            Work::Policy(e, r, p) => host.policy(e, &r, p).map(Value::Policy),
                            Work::Cleanup(e, r) => host.cleanup(e, r).map(Value::Cleanup),
                            Work::Start(r, e, q, p, f) => host.start(r, e, &q, &p, f).map(|()| Value::Unit),
                            Work::Poll(r) => host.poll(r).map(Value::Progress),
                            Work::Revoke(r) => host.revoke(r).map(|()| Value::Unit),
                            Work::Kill(r) => host.kill(r).map(|()| Value::Unit),
                            Work::Io(r) => host.take_io(r).map(Value::Io),
                            Work::Resize(r, s) => host.resize(r, s).map(|()| Value::Unit),
                            Work::Forget(r) => host.forget(r).map(|()| Value::Unit),
                        };
                        if completed.send(Completed { key, result, at: Instant::now() }).is_err() { break; }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {},
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
        Ok(Self { sender, receiver, slots: BTreeMap::new(), scanner })
    }
    fn receive(&mut self) {
        while let Ok(done) = self.receiver.try_recv() {
            if let Some(slot) = self.slots.get_mut(&done.key) { slot.result = Some((done.result, done.at)); }
        }
    }
    fn request(&mut self, key: Key, work: Work) -> Result<()> {
        self.receive();
        if let Some(slot) = self.slots.get(&key) {
            if slot.result.is_none() && slot.sent.elapsed() > Duration::from_secs(30) { return Err(ErrorCode::IsolationUnavailable); }
            return Ok(());
        }
        if self.slots.len() >= 1024 { return Err(ErrorCode::ResourceLimit); }
        self.sender.try_send((key.clone(), work)).map_err(|_| ErrorCode::IsolationUnavailable)?;
        self.slots.insert(key, Slot { sent: Instant::now(), result: None });
        Ok(())
    }
    fn take(&mut self, key: &Key) -> Result<Value> {
        let slot = self.slots.get_mut(key).ok_or(ErrorCode::NotReady)?;
        let Some((result, _)) = slot.result.take() else { return Err(ErrorCode::NotReady); };
        self.slots.remove(key);
        result
    }
    fn unit(&mut self, key: Key, work: Work, retain: bool) -> Result<()> {
        self.request(key.clone(), work)?;
        if retain {
            let slot = self.slots.get(&key).ok_or(ErrorCode::Internal)?;
            match &slot.result {
                Some((Ok(Value::Unit), _)) => return Ok(()),
                Some((Err(error), _)) => { let error = *error; self.slots.remove(&key); return Err(error); },
                _ => return Err(ErrorCode::NotReady),
            }
        }
        match self.take(&key)? { Value::Unit => Ok(()), _ => Err(ErrorCode::Internal) }
    }
}
impl Host for WorkerHost {
    type Inputs = Vec<OwnedFd>; type Io = Vec<OwnedFd>;
    fn ready(&mut self, e: EmployeeId) -> Result<()> {
        self.scanner.health(e)?;
        let key = Key::Ready(e);
        self.request(key.clone(), Work::Ready(e))?;
        if self.slots.get(&key).and_then(|s| s.result.as_ref()).is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2)) {
            self.slots.remove(&key); return Err(ErrorCode::NotReady);
        }
        match self.take(&key)? { Value::Unit => Ok(()), _ => Err(ErrorCode::Internal) }
    }
    fn policy(&mut self, e: EmployeeId, r: &RuntimeKind, p: bool) -> Result<LaunchPolicy> {
        let key = Key::Policy(e, serde_json::to_string(r).map_err(|_| ErrorCode::Internal)?, p);
        self.request(key.clone(), Work::Policy(e, r.clone(), p))?;
        match self.take(&key)? { Value::Policy(v) => Ok(v), _ => Err(ErrorCode::Internal) }
    }
    fn cleanup(&mut self, e: EmployeeId, r: Option<RunHandle>) -> Result<CleanupReadback> {
        let key = Key::Cleanup(e, r.map(|r| r.0));
        self.request(key.clone(), Work::Cleanup(e, r))?;
        if self.slots.get(&key).and_then(|s| s.result.as_ref()).is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2)) {
            self.slots.remove(&key); return Err(ErrorCode::NotReady);
        }
        match self.take(&key)? { Value::Cleanup(v) => Ok(v), _ => Err(ErrorCode::Internal) }
    }
    fn start(&mut self, r: RunHandle, e: EmployeeId, q: &SpawnRequest, p: &LaunchPolicy, f: Self::Inputs) -> Result<()> {
        self.slots.remove(&Key::Cleanup(e, None));
        self.scanner.register(r, e, p.startup_ms)?;
        self.request(Key::Start(r.0), Work::Start(r, e, q.clone(), p.clone(), f))
    }
    fn poll(&mut self, r: RunHandle) -> Result<HostProgress> {
        self.receive();
        if self.slots.contains_key(&Key::Start(r.0)) {
            match self.take(&Key::Start(r.0)) {
                Err(ErrorCode::NotReady) => return Ok(HostProgress::Starting),
                Ok(Value::Unit) => {}, Err(e) => return Err(e), _ => return Err(ErrorCode::Internal),
            }
        }
        let key = Key::Poll(r.0);
        self.request(key.clone(), Work::Poll(r))?;
        match self.take(&key) { Ok(Value::Progress(v)) => Ok(v), Err(ErrorCode::NotReady) => Err(ErrorCode::NotReady), Err(e) => Err(e), _ => Err(ErrorCode::Internal) }
    }
    fn revoke(&mut self, r: RunHandle) -> Result<()> { self.scanner.cancel(r)?; self.unit(Key::Revoke(r.0), Work::Revoke(r), true) }
    fn kill(&mut self, r: RunHandle) -> Result<()> { self.scanner.cancel(r)?; self.unit(Key::Kill(r.0), Work::Kill(r), true) }
    fn take_io(&mut self, r: RunHandle) -> Result<Self::Io> {
        let key = Key::Io(r.0); self.request(key.clone(), Work::Io(r))?;
        match self.take(&key)? { Value::Io(v) => Ok(v), _ => Err(ErrorCode::Internal) }
    }
    fn resize(&mut self, r: RunHandle, s: PtySize) -> Result<()> { self.unit(Key::Resize(r.0, s.columns, s.rows), Work::Resize(r, s), false) }
    fn detect_intruders(&mut self) -> Result<Vec<EmployeeId>> { self.scanner.scan() }
    fn forget(&mut self, r: RunHandle) -> Result<()> {
        self.unit(Key::Forget(r.0), Work::Forget(r), false)?;
        self.slots.retain(|key, _| match key {
            Key::Start(id) | Key::Poll(id) | Key::Revoke(id) | Key::Kill(id) | Key::Io(id) | Key::Resize(id, ..) | Key::Forget(id) => *id != r.0,
            Key::Cleanup(_, Some(id)) => *id != r.0, _ => true,
        });
        Ok(())
    }
}
