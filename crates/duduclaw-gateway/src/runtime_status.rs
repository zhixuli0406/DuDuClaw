//! Phase 8.5 — JSON status endpoint for the PTY pool runtime.
//!
//! Complements the Prometheus `/metrics` endpoint with a richer JSON
//! payload tailored for dashboard polling. Same security stance as
//! `/metrics`: localhost-only, no auth (loopback is the auth boundary).
//!
//! Endpoint: `GET /api/runtime/status`
//!
//! Response shape (stable JSON):
//! ```json
//! {
//!   "transport": "in_process" | "managed_worker",
//!   "kill_switch_active": bool,
//!   "retry_disabled": bool,
//!   "sessions": {
//!     "active": <u64>,
//!     "spawned_total": <u64>,
//!     "cache_hits_total": <u64>,
//!     "evicted_idle_total": <u64>,
//!     "evicted_unhealthy_total": <u64>,
//!     "evicted_shutdown_total": <u64>
//!   },
//!   "invokes": {
//!     "ok_total": <u64>,
//!     "empty_payload_total": <u64>,
//!     "error_total": <u64>,
//!     "timeout_total": <u64>,
//!     "duration_ms_sum": <u64>,
//!     "duration_ms_count": <u64>,
//!     "duration_ms_avg": <f64>
//!   },
//!   "worker": {
//!     "health_misses_total": <u64>,
//!     "restarts_total": <u64>
//!   }
//! }
//! ```

use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct RuntimeStatus {
    pub transport: &'static str,
    pub kill_switch_active: bool,
    pub retry_disabled: bool,
    pub sessions: SessionStats,
    pub invokes: InvokeStats,
    pub worker: WorkerStats,
}

#[derive(Debug, Serialize)]
pub struct SessionStats {
    pub active: usize,
    pub spawned_total: u64,
    pub cache_hits_total: u64,
    pub evicted_idle_total: u64,
    pub evicted_unhealthy_total: u64,
    pub evicted_shutdown_total: u64,
}

#[derive(Debug, Serialize)]
pub struct InvokeStats {
    pub ok_total: u64,
    pub empty_payload_total: u64,
    pub error_total: u64,
    pub timeout_total: u64,
    pub duration_ms_sum: u64,
    pub duration_ms_count: u64,
    pub duration_ms_avg: f64,
}

#[derive(Debug, Serialize)]
pub struct WorkerStats {
    pub health_misses_total: u64,
    pub restarts_total: u64,
}

/// Build the current `RuntimeStatus` snapshot. Pure read of the metrics
/// registry — safe to call from any context, no I/O.
pub fn snapshot() -> RuntimeStatus {
    let m = crate::metrics::global_metrics();
    let cache_hits = m.pty_pool_acquires_cache_hit_total.load(Ordering::Relaxed);
    let spawns = m.pty_pool_acquires_spawn_total.load(Ordering::Relaxed);

    let invoke_count: u64 = m
        .pty_pool_invoke_duration_buckets
        .iter()
        .map(|b| b.load(Ordering::Relaxed))
        .sum();
    let invoke_sum = m.pty_pool_invoke_duration_sum_ms.load(Ordering::Relaxed);
    let invoke_avg = if invoke_count > 0 {
        invoke_sum as f64 / invoke_count as f64
    } else {
        0.0
    };

    RuntimeStatus {
        transport: if crate::pty_runtime::is_managed_worker_active() {
            "managed_worker"
        } else {
            "in_process"
        },
        kill_switch_active: crate::pty_runtime::is_pty_pool_disabled_globally(),
        retry_disabled: crate::pty_runtime::is_pty_retry_disabled(),
        sessions: SessionStats {
            active: crate::pty_runtime::session_count(),
            spawned_total: spawns,
            cache_hits_total: cache_hits,
            evicted_idle_total: m.pty_pool_evicted_idle_total.load(Ordering::Relaxed),
            evicted_unhealthy_total: m.pty_pool_evicted_unhealthy_total.load(Ordering::Relaxed),
            evicted_shutdown_total: m.pty_pool_evicted_shutdown_total.load(Ordering::Relaxed),
        },
        invokes: InvokeStats {
            ok_total: m.pty_pool_invokes_ok_total.load(Ordering::Relaxed),
            empty_payload_total: m.pty_pool_invokes_empty_total.load(Ordering::Relaxed),
            error_total: m.pty_pool_invokes_error_total.load(Ordering::Relaxed),
            timeout_total: m.pty_pool_invokes_timeout_total.load(Ordering::Relaxed),
            duration_ms_sum: invoke_sum,
            duration_ms_count: invoke_count,
            duration_ms_avg: invoke_avg,
        },
        worker: WorkerStats {
            health_misses_total: m.worker_health_misses_total.load(Ordering::Relaxed),
            restarts_total: m.worker_restarts_total.load(Ordering::Relaxed),
        },
    }
}

/// Axum handler. Localhost-only; non-loopback peers get 403.
pub async fn handler(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> axum::response::Response {
    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            "Runtime status only available from localhost",
        )
            .into_response();
    }
    let body = snapshot();
    (StatusCode::OK, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_renders_known_fields() {
        // The metrics registry is a process-wide singleton; we can't
        // reset it, so the test just verifies the shape rather than
        // exact values.
        let s = snapshot();
        assert!(matches!(s.transport, "in_process" | "managed_worker"));
        // Avg is non-negative.
        assert!(s.invokes.duration_ms_avg >= 0.0);
    }

    #[test]
    fn snapshot_avg_zero_when_no_invokes() {
        // After a fresh registry, both sum and count are 0 → avg is 0.0.
        // We can't construct a fresh registry from outside, but we know
        // the type semantics — if count is 0 the function MUST return
        // exactly 0.0 (not NaN).
        let s = snapshot();
        if s.invokes.duration_ms_count == 0 {
            assert_eq!(s.invokes.duration_ms_avg, 0.0);
        }
    }

    #[test]
    fn snapshot_serialises_to_stable_json_keys() {
        // Lock the wire shape so dashboard authors can rely on field names.
        let s = snapshot();
        let json = serde_json::to_string(&s).unwrap();
        for key in [
            "\"transport\"",
            "\"kill_switch_active\"",
            "\"retry_disabled\"",
            "\"sessions\"",
            "\"active\"",
            "\"spawned_total\"",
            "\"cache_hits_total\"",
            "\"invokes\"",
            "\"ok_total\"",
            "\"empty_payload_total\"",
            "\"duration_ms_avg\"",
            "\"worker\"",
            "\"health_misses_total\"",
            "\"restarts_total\"",
        ] {
            assert!(json.contains(key), "missing key {key} in {json}");
        }
    }
}
