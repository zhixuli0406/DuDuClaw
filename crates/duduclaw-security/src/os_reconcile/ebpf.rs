//! Linux eBPF observer (stage C of P3-3) — stub only.
//!
//! The intended implementation attaches tracepoints via `aya` / `libbpf-rs`
//! (or a `fanotify` + `/proc/<pid>/net` best-effort fallback) to capture file
//! writes and outbound connections for an agent subprocess. That requires
//! `CAP_BPF` / root and a Linux host, so it is out of scope for stage A/B.
//!
//! This stub keeps the observer surface uniform across platforms: it never
//! captures anything and always reports [`Availability::Unsupported`], so the
//! reconciliation loop degrades to a tool-calls-only self-consistency check on
//! Linux until stage C lands.

use super::{ActionObserver, Availability, OsEvent};

/// Placeholder Linux eBPF observer (not implemented — stage C).
#[derive(Debug, Default)]
pub struct EbpfObserver;

impl EbpfObserver {
    pub fn new() -> Self {
        Self
    }
}

impl ActionObserver for EbpfObserver {
    fn collect_since(&self, _pid: u32, _since: &str) -> Result<Vec<OsEvent>, String> {
        Err("Linux eBPF observer not implemented (P3-3 stage C)".to_string())
    }

    fn availability(&self) -> Availability {
        Availability::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_unsupported_and_errs() {
        let obs = EbpfObserver::new();
        assert_eq!(obs.availability(), Availability::Unsupported);
        assert!(obs.collect_since(1, "2026-07-06T12:00:00Z").is_err());
    }
}
