//! Limits belong to one registered socketpair/run and survive tunnel closure.
//!
//! R8z-6: per-second RATES (`frames_per_second`, `bytes_per_second`) throttle
//! the run's payload relay: [`RunBudget::throttle`] waits until the token
//! buckets hold the next frame, bounded by the idle timeout. Only the hard
//! caps terminate: `total_bytes`, `total_connections`, `max_tunnels`, a frame
//! that can never fit a bucket, or a wait that would outlast the idle bound.
//! The control socket's registration budget still refuses at once
//! ([`RunBudget::frame`], `daemon.rs`).
use std::time::{Duration, Instant};
use duduclaw_isolation::egress::{EgressError, EgressLimits};

// One daemon-wide ceiling covers pending DNS/connect and established tunnels.
// A separate configuration bound limits per-run bookkeeping reservations.
pub const MAX_GLOBAL_TUNNELS: usize = 64;
pub const MAX_CONFIGURED_TUNNELS: u32 = 1024;

/// R8z-6: fixed, closed sub-reason of a run record's `terminal_code`. Journal
/// only (the run record); never on the gateway/spawnd/forwarder wire. Labels
/// carry no host, address, tunnel or content information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalDetail {
    /// `resource_limit`: one frame could not be admitted within the idle bound.
    FrameRate,
    /// `resource_limit`: one frame's bytes could not be admitted within the idle bound.
    ByteRate,
    /// `resource_limit`: the run's `total_bytes` cap (both directions).
    TotalBytes,
    /// `resource_limit`: the run's `total_connections` cap.
    TotalConnections,
    /// `resource_limit`: the profile's `max_tunnels` (open at once).
    MaxTunnels,
    /// `invalid_request`: a frame length above `max_frame_bytes`.
    FrameTooLarge,
    /// `resource_limit`: the profile limits or run caps are invalid.
    InvalidLimits,
    /// `invalid_request`: a frame that does not decode.
    InvalidFrame,
    /// `invalid_request`: a valid frame in the wrong direction or state.
    ProtocolState,
    /// `timeout`: no frame in either direction for `idle_timeout_ms`.
    IdleTimeout,
    /// `timeout`: a started frame was not completed within `idle_timeout_ms`.
    FrameTimeout,
    /// `timeout`: the forwarder did not accept a frame within `idle_timeout_ms`.
    WriteTimeout,
    /// `unavailable`: the forwarder closed the run socket (normal end included).
    PeerClosed,
    /// `unavailable`: spawnd unregistered the run or the controller went away.
    Revoked,
    /// Any other daemon-internal failure.
    Internal,
}

/// A terminal code with its fixed detail. Converts to the wire code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal { pub code: EgressError, pub detail: TerminalDetail }
impl Terminal {
    pub const fn new(code: EgressError, detail: TerminalDetail) -> Self { Self { code, detail } }
    pub const fn limit(detail: TerminalDetail) -> Self { Self::new(EgressError::ResourceLimit, detail) }
}
impl From<Terminal> for EgressError {
    fn from(terminal: Terminal) -> Self { terminal.code }
}
/// Derivation from the code alone, for sites without a more specific detail.
impl From<EgressError> for Terminal {
    fn from(code: EgressError) -> Self {
        let detail = match code {
            EgressError::InvalidRequest => TerminalDetail::ProtocolState,
            EgressError::Timeout => TerminalDetail::IdleTimeout,
            EgressError::Unavailable => TerminalDetail::PeerClosed,
            EgressError::ResourceLimit | EgressError::DestinationDenied | EgressError::PortDenied => TerminalDetail::Internal,
        };
        Self { code, detail }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCaps {
    pub total_bytes: u64,
    pub total_connections: u32,
}
impl RunCaps {
    pub fn validate(self) -> Result<(), EgressError> {
        if self.total_bytes == 0 || self.total_connections == 0 || self.total_connections > 65536 { Err(EgressError::ResourceLimit) } else { Ok(()) }
    }
}

const NANOS: u64 = 1_000_000_000;
// A token bucket limits bursts to one second and replenishes continuously.
// Amounts are in nano-tokens; `rate` nano-tokens arrive per nanosecond.
struct Bucket { available: u64, capacity: u64, rate: u64, last: Instant }
impl Bucket {
    fn new(per_sec: u32, now: Instant) -> Self {
        let rate = u64::from(per_sec);
        let capacity = rate * NANOS;
        Self { available: capacity, capacity, rate, last: now }
    }
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_nanos();
        let refill = elapsed.saturating_mul(u128::from(self.rate));
        self.available = self.available.saturating_add(refill.min(u128::from(u64::MAX)) as u64).min(self.capacity);
        // An earlier injected `now` never rewinds the clock (no double refill).
        if now > self.last { self.last = now; }
    }
    /// Time until `count` tokens are available; `None` if they never can be
    /// (more than one second's worth, the bucket capacity).
    fn wait_for(&mut self, count: u64, now: Instant) -> Option<Duration> {
        self.refill(now);
        let cost = count.checked_mul(NANOS)?;
        if cost > self.capacity { return None; }
        let missing = cost.saturating_sub(self.available);
        Some(Duration::from_nanos(missing.div_ceil(self.rate)))
    }
    fn take(&mut self, count: u64, now: Instant) -> bool {
        if self.wait_for(count, now) != Some(Duration::ZERO) { return false; }
        // wait_for checked the multiplication and the availability.
        self.available -= count * NANOS;
        true
    }
}
pub struct RunBudget {
    frames: Bucket,
    bytes: Bucket,
    caps: RunCaps,
    total_bytes: u64,
    total_connections: u32,
    active: u16,
    max_active: u16,
}
impl RunBudget {
    pub fn new(limits: &EgressLimits, caps: RunCaps, now: Instant) -> Result<Self, EgressError> {
        limits.validate().map_err(|_| EgressError::ResourceLimit)?;
        caps.validate()?;
        Ok(Self { frames: Bucket::new(limits.frames_per_second.get(), now),
            bytes: Bucket::new(limits.bytes_per_second.get(), now), caps, total_bytes: 0,
            total_connections: 0, active: 0, max_active: limits.max_tunnels.get() })
    }
    /// Hard refusal when the frame bucket is empty. Control plane only
    /// (spawnd registrations); the run relay uses [`Self::throttle`].
    pub fn frame(&mut self, now: Instant) -> Result<(), Terminal> {
        if self.frames.take(1, now) { Ok(()) } else { Err(Terminal::limit(TerminalDetail::FrameRate)) }
    }
    /// Shared across both directions; called before writing any payload bytes.
    /// Hard refusal when the byte bucket is empty; the relay uses [`Self::throttle`].
    pub fn bytes(&mut self, count: usize, now: Instant) -> Result<(), Terminal> {
        let total = self.checked_total(count)?;
        if !self.bytes.take(count as u64, now) { return Err(Terminal::limit(TerminalDetail::ByteRate)); }
        self.total_bytes = total;
        Ok(())
    }
    fn checked_total(&self, count: usize) -> Result<u64, Terminal> {
        self.total_bytes.checked_add(count as u64).filter(|total| *total <= self.caps.total_bytes)
            .ok_or(Terminal::limit(TerminalDetail::TotalBytes))
    }
    /// Time until one more frame fits the frame bucket; `None` never.
    pub fn frame_wait_for(&mut self, now: Instant) -> Option<Duration> { self.frames.wait_for(1, now) }
    /// Time until `count` bytes fit the byte bucket; `None` never (more than
    /// one second's worth).
    pub fn bytes_wait_for(&mut self, count: usize, now: Instant) -> Option<Duration> {
        self.bytes.wait_for(count as u64, now)
    }
    /// Relay path: the wait before one frame carrying `count` payload bytes is
    /// admitted, and which rate binds. The hard `total_bytes` cap and a frame
    /// no bucket can ever hold fail at once. Nothing is taken.
    pub fn relay_wait(&mut self, count: usize, now: Instant) -> Result<(Duration, TerminalDetail), Terminal> {
        self.checked_total(count)?;
        let frame = self.frame_wait_for(now).ok_or(Terminal::limit(TerminalDetail::FrameRate))?;
        let bytes = if count == 0 { Duration::ZERO }
            else { self.bytes_wait_for(count, now).ok_or(Terminal::limit(TerminalDetail::ByteRate))? };
        Ok(if bytes > frame { (bytes, TerminalDetail::ByteRate) } else { (frame, TerminalDetail::FrameRate) })
    }
    /// Relay path: take one frame and `count` bytes, all or nothing.
    pub fn relay_take(&mut self, count: usize, now: Instant) -> Result<(), Terminal> {
        let (wait, binding) = self.relay_wait(count, now)?;
        if !wait.is_zero() { return Err(Terminal::limit(binding)); }
        self.frame(now)?;
        if count > 0 { self.bytes(count, now)?; }
        Ok(())
    }
    /// Relay backpressure: the caller reads nothing more from that side while
    /// this waits. Sleeps until the next frame (and its bytes) fits, at most
    /// `bound` (the idle timeout) in total; a longer wait terminates with the
    /// binding rate as detail. Hard caps terminate immediately.
    pub async fn throttle(&mut self, count: usize, bound: Duration) -> Result<(), Terminal> {
        let started = Instant::now();
        loop {
            let now = Instant::now();
            let (wait, binding) = self.relay_wait(count, now)?;
            if wait.is_zero() { return self.relay_take(count, now); }
            if now.saturating_duration_since(started).saturating_add(wait) > bound { return Err(Terminal::limit(binding)); }
            tokio::time::sleep(wait).await;
        }
    }
    /// Attempts count even when DNS/connect fails. Closing never refunds totals.
    pub fn open(&mut self) -> Result<(), Terminal> {
        if self.active >= self.max_active { return Err(Terminal::limit(TerminalDetail::MaxTunnels)); }
        if self.total_connections >= self.caps.total_connections { return Err(Terminal::limit(TerminalDetail::TotalConnections)); }
        self.active += 1;
        self.total_connections += 1;
        Ok(())
    }
    pub fn close(&mut self) { self.active = self.active.saturating_sub(1); }
    pub fn totals(&self) -> (u64, u32) { (self.total_bytes, self.total_connections) }
}

pub fn default_limits() -> EgressLimits {
    EgressLimits {
        max_frame_bytes: 65536.try_into().unwrap(), max_tunnels: 16.try_into().unwrap(),
        frames_per_second: 1024.try_into().unwrap(), bytes_per_second: (4 * 1024 * 1024).try_into().unwrap(),
        connect_timeout_ms: 5000.try_into().unwrap(), idle_timeout_ms: 30000.try_into().unwrap(),
        max_header_bytes: 8192.try_into().unwrap(), max_headers: 32.try_into().unwrap(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn limit(detail: TerminalDetail) -> Result<(), Terminal> { Err(Terminal::limit(detail)) }
    #[test]
    fn limits_are_shared_and_not_refunded_by_close() {
        let now = Instant::now();
        let mut limits = default_limits();
        limits.max_tunnels = 1.try_into().unwrap();
        limits.bytes_per_second = 10.try_into().unwrap();
        limits.frames_per_second = 2.try_into().unwrap();
        let mut b = RunBudget::new(&limits, RunCaps { total_bytes: 12, total_connections: 2 }, now).unwrap();
        b.open().unwrap(); assert_eq!(b.open(), limit(TerminalDetail::MaxTunnels));
        b.close(); b.open().unwrap(); b.close(); assert_eq!(b.open(), limit(TerminalDetail::TotalConnections));
        // The hard (control-plane) calls still refuse at once on an empty bucket.
        b.bytes(10, now).unwrap(); assert_eq!(b.bytes(1, now), limit(TerminalDetail::ByteRate));
        b.bytes(2, now + Duration::from_secs(1)).unwrap();
        assert_eq!(b.bytes(1, now + Duration::from_secs(2)), limit(TerminalDetail::TotalBytes));
        b.frame(now).unwrap(); b.frame(now).unwrap(); assert_eq!(b.frame(now), limit(TerminalDetail::FrameRate));
        b.frame(now + Duration::from_millis(500)).unwrap();
        assert_eq!(b.totals(), (12,2));
    }

    #[test]
    fn wait_for_reports_the_refill_time_deterministically() {
        let now = Instant::now();
        let mut limits = default_limits();
        limits.bytes_per_second = 1000.try_into().unwrap();
        limits.frames_per_second = 4.try_into().unwrap();
        let mut b = RunBudget::new(&limits, RunCaps { total_bytes: 1 << 20, total_connections: 1 }, now).unwrap();
        assert_eq!(b.bytes_wait_for(1000, now), Some(Duration::ZERO));
        b.relay_take(1000, now).unwrap();
        // Empty byte bucket: 250 bytes need 250 ms at 1000 B/s; nothing was taken.
        assert_eq!(b.bytes_wait_for(250, now), Some(Duration::from_millis(250)));
        assert_eq!(b.relay_wait(250, now), Ok((Duration::from_millis(250), TerminalDetail::ByteRate)));
        assert_eq!(b.relay_take(250, now), limit(TerminalDetail::ByteRate));
        assert_eq!(b.relay_wait(250, now + Duration::from_millis(100)), Ok((Duration::from_millis(150), TerminalDetail::ByteRate)));
        b.relay_take(250, now + Duration::from_millis(250)).unwrap();
        // More than one second's worth can never fit.
        assert_eq!(b.bytes_wait_for(1001, now), None);
        assert_eq!(b.relay_wait(1001, now), Err(Terminal::limit(TerminalDetail::ByteRate)));
        // Frames: 4/s; 3 left after the take at +250 ms. Three more, then 250 ms per frame.
        let later = now + Duration::from_millis(250);
        for _ in 0..3 { b.relay_take(0, later).unwrap(); }
        assert_eq!(b.frame_wait_for(later), Some(Duration::from_millis(250)));
        assert_eq!(b.relay_wait(0, later), Ok((Duration::from_millis(250), TerminalDetail::FrameRate)));
        // An earlier injected clock never rewinds the bucket.
        assert_eq!(b.frame_wait_for(now), Some(Duration::from_millis(250)));
        assert_eq!(b.totals(), (1250, 0));
    }

    #[test]
    fn a_256_kib_upload_fits_the_default_rates_without_waiting() {
        let now = Instant::now();
        let mut b = RunBudget::new(&default_limits(), RunCaps { total_bytes: 1 << 32, total_connections: 4096 }, now).unwrap();
        // 32 frames of the 8 KiB protocol chunk, all at the same instant.
        for _ in 0..32 { assert_eq!(b.relay_wait(8192, now).unwrap().0, Duration::ZERO); b.relay_take(8192, now).unwrap(); }
        assert_eq!(b.totals().0, 256 * 1024);
    }

    #[test]
    fn total_bytes_is_hard_even_when_the_rate_would_wait() {
        let now = Instant::now();
        let mut limits = default_limits();
        limits.bytes_per_second = 100.try_into().unwrap();
        let mut b = RunBudget::new(&limits, RunCaps { total_bytes: 150, total_connections: 1 }, now).unwrap();
        b.relay_take(100, now).unwrap();
        assert_eq!(b.relay_wait(51, now), Err(Terminal::limit(TerminalDetail::TotalBytes)));
        assert_eq!(b.relay_wait(50, now), Ok((Duration::from_millis(500), TerminalDetail::ByteRate)));
    }

    #[tokio::test]
    async fn throttle_delays_an_over_rate_upload_instead_of_failing() {
        let mut limits = default_limits();
        limits.bytes_per_second = 1000.try_into().unwrap();
        let mut b = RunBudget::new(&limits, RunCaps { total_bytes: 1 << 20, total_connections: 1 }, Instant::now()).unwrap();
        let started = Instant::now();
        // 1000 bytes empty the bucket; the next 100 wait ≈ 100 ms, then pass.
        b.throttle(1000, Duration::from_secs(1)).await.unwrap();
        b.throttle(100, Duration::from_secs(1)).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(90));
        assert_eq!(b.totals().0, 1100);
        // A wait beyond the idle bound terminates with the binding rate.
        assert_eq!(b.throttle(900, Duration::from_millis(50)).await, limit(TerminalDetail::ByteRate));
        assert_eq!(b.totals().0, 1100);
    }
}
