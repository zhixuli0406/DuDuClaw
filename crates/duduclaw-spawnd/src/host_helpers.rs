//! HX4 helper glue of `host::Core` on the lifecycle thread: linking the
//! per-run engine, per-call start/signal, events and content-free audit.
//! Split out of `host.rs` in the HX security fix round (file size).
use super::*;

/// Content-free helper audit (helper index, slot, sizes, outcome, reasons).
pub(super) fn emit_helper(audit: &AuditWorker, employee: EmployeeId, run: RunId, fact: HelperAudit) {
    let (event, detail) = match fact {
        HelperAudit::Started { call, index, slot, input_bytes, effective_timeout_ms } => (Event::HelperStarted,
            Detail::Helper { call, helper: index, slot, input_bytes, effective_timeout_ms: Some(effective_timeout_ms), outcome: None,
                reason: None, drained: None }),
        HelperAudit::Ended { call, index, slot, input_bytes, outcome, reason, drained } => (Event::HelperEnded,
            Detail::Helper { call, helper: index, slot, input_bytes, effective_timeout_ms: None, outcome: Some(outcome),
                reason: Some(reason), drained: Some(drained) }),
        HelperAudit::Closed { code, reason } => (Event::HelperChannelClosed, Detail::HelperClosed { code, reason }),
    };
    emit_detail(audit, event, Some(employee), Some(run), Some(detail));
}

impl Core {
    /// The launcher link moves out of the trampoline once, when Running. The
    /// engine gets the run's MCP revoker for the runtime-exit revocation.
    pub(super) fn link_helpers(&mut self, run: RunHandle) -> Result<()> {
        let link = self.components.trampoline.take_helper_link(run);
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        entry.helper = Some(match link {
            Ok(link) => match HelperEngine::new(run, link) {
                Ok(engine) => RunHelper::Engine(Box::new(engine.with_mcp_revoker(entry.mcp_revoker.clone()))),
                Err(code) => RunHelper::Unavailable(code),
            },
            Err(code) => RunHelper::Unavailable(code),
        });
        Ok(())
    }
    /// Launcher frames, spawnd's watchdog and the runtime cgroup, every turn.
    pub(super) fn service_helpers(&mut self) {
        let Ok(now) = crate::server::coordination_time_ms() else { return };
        for (run, entry) in &mut self.runs {
            if entry.closed { continue; }
            let Some(RunHelper::Engine(engine)) = entry.helper.as_mut() else { continue };
            engine.service(now);
            for fact in engine.take_audit() { emit_helper(&self.audit, entry.employee, *run, fact); }
            // Finding 4: an MCP channel that could not be revoked when the
            // runtime exited fences the whole run (fail closed).
            if engine.mcp_revoke_failed() {
                match self.monitor.cancellation(RunHandle(*run)) {
                    Ok(fence) => fence.store(true, Ordering::Release),
                    Err(_) => self.poisoned = true,
                }
            }
        }
        // HX6 finding 4: start / read this host's recording probe (no-op while
        // the build kill switch is closed).
        self.service_recording_probe();
    }
    pub(super) fn run_helper(&mut self, job: HelperStart<Vec<OwnedFd>>) -> Result<HelperStarted<Vec<OwnedFd>>> {
        if self.poisoned { return Err(ErrorCode::HelperUnavailable); }
        let HelperStart { run, employee, call, helper, slot, timeout_ms, input_bytes, mut input, run_remaining_ms } = job;
        let input = match (input.pop(), input.is_empty()) { (Some(fd), true) => fd, _ => return Err(ErrorCode::InvalidFds) };
        let cancelled = self.monitor.cancellation(run).map_err(|_| ErrorCode::NotReady)?;
        let entry = self.runs.get(&run.0).ok_or(ErrorCode::NotFound)?;
        // [F7] root's own record of the run's employee must be the coordinator's.
        if entry.employee != employee { return Err(ErrorCode::Unauthorized); }
        if entry.closed || !entry.started_event || cancelled.load(Ordering::Acquire) { return Err(ErrorCode::NotReady); }
        let parent = entry.helper_parent.as_ref().ok_or(ErrorCode::HelperUnavailable)?;
        let policy = self.policy.as_ref().ok_or(ErrorCode::HelperUnavailable)?;
        // §2.3: re-derived per call from the run's employee (entitlement may
        // have been withdrawn since start), never from the request.
        let derived = policy.derive_helper(entry.employee, parent, &helper).map_err(|_| ErrorCode::RuntimeDenied)?;
        if derived.binding.slot != slot { return Err(ErrorCode::InvalidRequest); }
        // Finding 1: the engine refuses unless this identity equals the one
        // the run's table was built from (a reload can reuse the index).
        let binding_digest = crate::helper_policy::binding_digest(derived.binding).ok_or(ErrorCode::RuntimeDenied)?;
        // Root's own deadline bounds the coordinator's view.
        let own = u64::try_from(entry.runtime_deadline.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(u64::MAX);
        let plan = CallPlan { call, index: derived.index, slot, runtime: derived.runtime.runtime.clone(),
            profile: derived.binding.profile, binding_digest, requested_ms: timeout_ms, input_bytes,
            run_remaining_ms: run_remaining_ms.min(own), termination_grace_ms: parent.timeouts.termination_grace_ms };
        let now = os(crate::server::coordination_time_ms())?;
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        let engine = match entry.helper.as_mut() {
            Some(RunHelper::Engine(engine)) => engine,
            Some(RunHelper::Unavailable(code)) => return Err(*code),
            None => return Err(ErrorCode::NotReady),
        };
        let started = engine.start(now, plan, input);
        for fact in engine.take_audit() { emit_helper(&self.audit, employee, run.0, fact); }
        let started = started?;
        Ok(HelperStarted { effective_timeout_ms: started.effective_timeout_ms, pipes: vec![started.stdout, started.stderr] })
    }
    pub(super) fn signal_helper(&mut self, run: RunHandle, call: HelperCallId, signal: HelperSignal) -> Result<()> {
        let entry = self.runs.get_mut(&run.0).ok_or(ErrorCode::NotFound)?;
        let Some(RunHelper::Engine(engine)) = entry.helper.as_mut() else { return Ok(()) };
        let result = engine.signal(call, signal);
        for fact in engine.take_audit() { emit_helper(&self.audit, entry.employee, run.0, fact); }
        result
    }
    pub(super) fn collect_helper_events(&mut self) -> Vec<HelperEvent> {
        let mut events = std::mem::take(&mut self.helper_events);
        for entry in self.runs.values_mut() {
            if let Some(RunHelper::Engine(engine)) = entry.helper.as_mut() { events.extend(engine.take_events()); }
        }
        // The probe's end also reaches the coordinator, which ignores a call it never started.
        self.observe_recording_probe(&events);
        events
    }
}
