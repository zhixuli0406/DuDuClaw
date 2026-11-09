//! Independent UID monitor and its scan reader for `host.rs` (moved here
//! unchanged in the HX security fix round to keep `host.rs` below 800 lines;
//! only visibility widened to `pub(super)`).
use super::*;

#[derive(Clone)]
pub(super) struct ScanEntry { pub(super) account: EmployeeAccount, pub(super) run: Option<RunId>, pub(super) cgroup: Option<String>,
    pub(super) revokers: Vec<Arc<ConnectionRevoker>>, pub(super) cancelled: Arc<AtomicBool>, pub(super) deadline: Option<Instant> }
pub(super) struct ScanState { pub(super) at: Instant, pub(super) result: Result<Vec<EmployeeId>> }
pub(super) struct Monitor { pub(super) entries: Arc<Mutex<Vec<ScanEntry>>>, pub(super) state: Arc<Mutex<ScanState>>,
    pub(super) stop: Arc<AtomicBool> }
impl Monitor {
    pub(super) fn start(accounts: &[EmployeeAccount], audit: AuditWorker) -> io::Result<Self> {
        let entries = Arc::new(Mutex::new(accounts.iter().cloned().map(|account|
            ScanEntry { account, run: None, cgroup: None, revokers: vec![], cancelled: Arc::new(AtomicBool::new(false)), deadline: None }).collect::<Vec<_>>()));
        let state = Arc::new(Mutex::new(ScanState { at: Instant::now(), result: Err(unavailable()) }));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_entries, worker_state, worker_stop) = (entries.clone(), state.clone(), stop.clone());
        let (kill_sender, kill_receiver) = std::sync::mpsc::sync_channel::<RunId>(128);
        std::thread::Builder::new().name("spawnd-emergency-stop".into()).spawn(move || {
            while let Ok(run) = kill_receiver.recv() {
                if let Ok(systemd) = Systemd::connect() { let _ = systemd.kill_run(run); }
            }
        })?;
        std::thread::Builder::new().name("spawnd-uid-monitor".into()).spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let snapshot = match worker_entries.lock() { Ok(entries) => entries.clone(), Err(_) => break };
                let result = (|| -> Result<Vec<EmployeeId>> {
                    let mut affected = Vec::new();
                    let mut scan_failed = false;
                    for entry in snapshot {
                        let intruders = os(accounts::unexpected_processes(entry.account.uid, entry.cgroup.as_deref()));
                        // Capture cancellation candidates before validating the
                        // generation. Never rescan this UID after dropping the
                        // map lock: a later admission may already own it.
                        let all_processes = accounts::unexpected_processes(entry.account.uid, None);
                        // Re-read the admission generation after the /proc scan.
                        // Compare before acting, then release the map lock. The
                        // process list is already fixed; pidfd/start-time checks
                        // below reject reuse. Unit kills name this immutable run,
                        // so they cannot terminate a later admission generation.
                        let current = worker_entries.lock().map_err(|_| unavailable())?;
                        let live = current.iter().find(|v| v.account.employee == entry.account.employee).ok_or_else(unavailable)?;
                        if live.run != entry.run || live.cgroup != entry.cgroup { continue; }
                        let entry = live.clone();
                        if entry.deadline.is_some_and(|until| Instant::now() >= until)
                            || all_processes.is_err() || intruders.as_ref().is_err() || intruders.as_ref().is_ok_and(|p| !p.is_empty()) {
                            entry.cancelled.store(true, Ordering::Release);
                        }
                        let cancelled = entry.cancelled.load(Ordering::Acquire);
                        drop(current);
                        if cancelled {
                            for revoker in &entry.revokers { let _ = revoker.revoke(); }
                            // PID 1 operations never delay the scanner. Independently
                            // kill every employee process with pidfds immediately;
                            // the queued unit stop also kills root/runsup helpers.
                            if let Some(run) = entry.run { let _ = kill_sender.try_send(run); }
                            match all_processes {
                                Ok(processes) => for process in processes {
                                    scan_failed |= accounts::terminate_intruder(&process, entry.account.uid, None).is_err();
                                },
                                Err(_) => scan_failed = true,
                            }
                        }
                        let intruders = match intruders { Ok(value) => value, Err(_) => { scan_failed = true; continue; } };
                        if !intruders.is_empty() {
                            affected.push(entry.account.employee);
                            emit(&audit, Event::Intruder, Some(entry.account.employee), entry.run);
                            for process in intruders { scan_failed |= accounts::terminate_intruder(&process, entry.account.uid, entry.cgroup.as_deref()).is_err(); }
                        }
                    }
                    if scan_failed { Err(unavailable()) } else { Ok(affected) }
                })();
                if result.is_err() {
                    // Fence every run on an unreadable host scan, including runs
                    // whose UID was after the failed entry in the inventory.
                    if let Ok(entries) = worker_entries.lock() {
                        for entry in entries.iter() {
                            entry.cancelled.store(true, Ordering::Release);
                            for revoker in &entry.revokers { let _ = revoker.revoke(); }
                            if let Some(run) = entry.run { let _ = kill_sender.try_send(run); }
                        }
                    }
                }
                if let Ok(mut state) = worker_state.lock() {
                    // Sticky affected identities until the control loop observes them.
                    let result = match (&state.result, result) {
                        (Ok(old), Ok(mut new)) => { new.extend(old); new.sort(); new.dedup(); Ok(new) },
                        (_, other) => other,
                    };
                    *state = ScanState { at: started, result };
                }
                // Schedule at <=1 second, leave margin for the two-second fence.
                while started.elapsed() < Duration::from_secs(1) && !worker_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        })?;
        Ok(Self { entries, state, stop })
    }
    pub(super) fn cancellation(&self, run: RunHandle) -> Result<Arc<AtomicBool>> {
        let entries = self.entries.lock().map_err(|_| unavailable())?;
        Ok(entries.iter().find(|e| e.run == Some(run.0)).ok_or_else(unavailable)?.cancelled.clone())
    }
    pub(super) fn check_run(&self, run: RunHandle) -> Result<()> {
        if self.cancellation(run)?.load(Ordering::Acquire) { Err(unavailable()) } else { Ok(()) }
    }
    pub(super) fn running(&self, run: RunHandle, deadline: Instant) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        entries.iter_mut().find(|e| e.run == Some(run.0)).ok_or_else(unavailable)?.deadline = Some(deadline);
        Ok(())
    }
    pub(super) fn set_run(&self, employee: EmployeeId, run: Option<RunId>, cgroup: Option<String>, revokers: Vec<Arc<ConnectionRevoker>>) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| unavailable())?;
        let entry = entries.iter_mut().find(|entry| entry.account.employee == employee).ok_or_else(unavailable)?;
        if run.is_some() && entry.run != run { return Err(unavailable()); }
        entry.run = run; entry.cgroup = cgroup; entry.revokers = revokers;
        if run.is_none() { entry.deadline = None; }
        Ok(())
    }
}
pub(super) struct ScanReader { pub(super) state: Arc<Mutex<ScanState>>, pub(super) entries: Arc<Mutex<Vec<ScanEntry>>> }
impl Scan for ScanReader {
    fn health(&mut self, employee: EmployeeId) -> Result<()> {
        let state = self.state.try_lock().map_err(|_| unavailable())?;
        if state.at.elapsed() >= Duration::from_secs(2) { return Err(unavailable()); }
        if state.result.as_ref().map_err(|e| *e)?.contains(&employee) { return Err(unavailable()); }
        Ok(())
    }
    fn register(&mut self, run: RunHandle, employee: EmployeeId, startup_ms: u64) -> Result<()> {
        let mut entries = self.entries.try_lock().map_err(|_| unavailable())?;
        let entry = entries.iter_mut().find(|v| v.account.employee == employee).ok_or_else(unavailable)?;
        if entry.run.is_some() { return Err(unavailable()); }
        entry.run = Some(run.0); entry.cancelled = Arc::new(AtomicBool::new(false));
        entry.deadline = Some(Instant::now() + Duration::from_millis(startup_ms));
        Ok(())
    }
    fn cancel(&mut self, run: RunHandle) -> Result<()> {
        let entries = self.entries.try_lock().map_err(|_| unavailable())?;
        if let Some(entry) = entries.iter().find(|v| v.run == Some(run.0)) {
            entry.cancelled.store(true, Ordering::Release);
            for revoker in &entry.revokers { os(revoker.revoke())?; }
        }
        Ok(())
    }
    fn scan(&mut self) -> Result<Vec<EmployeeId>> {
        let mut state = self.state.try_lock().map_err(|_| unavailable())?;
        if state.at.elapsed() >= Duration::from_secs(2) { return Err(unavailable()); }
        let result = state.result.clone();
        if result.is_ok() { state.result = Ok(vec![]); }
        result
    }
}
impl Drop for Monitor { fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); } }
