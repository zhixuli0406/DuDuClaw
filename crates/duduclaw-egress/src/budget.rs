//! Limits belong to one registered socketpair/run and survive tunnel closure.
use std::time::Instant;
use duduclaw_isolation::egress::{EgressError, EgressLimits};

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

// A token bucket limits bursts to one second and replenishes continuously.
struct Bucket { available: u64, capacity: u64, last: Instant }
impl Bucket {
    fn new(per_sec: u32, now: Instant) -> Self {
        let capacity = u64::from(per_sec) * 1_000_000_000;
        Self { available: capacity, capacity, last: now }
    }
    fn take(&mut self, count: u64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_nanos();
        let refill = elapsed.saturating_mul(u128::from(self.capacity / 1_000_000_000));
        self.available = self.available.saturating_add(refill.min(u128::from(u64::MAX)) as u64).min(self.capacity);
        self.last = now;
        let Some(cost) = count.checked_mul(1_000_000_000) else { return false; };
        if cost > self.available { return false; }
        self.available -= cost;
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
    pub fn frame(&mut self, now: Instant) -> Result<(), EgressError> {
        if self.frames.take(1, now) { Ok(()) } else { Err(EgressError::ResourceLimit) }
    }
    /// Shared across both directions; called before writing any payload bytes.
    pub fn bytes(&mut self, count: usize, now: Instant) -> Result<(), EgressError> {
        let total = self.total_bytes.checked_add(count as u64).ok_or(EgressError::ResourceLimit)?;
        if total > self.caps.total_bytes || !self.bytes.take(count as u64, now) { return Err(EgressError::ResourceLimit); }
        self.total_bytes = total;
        Ok(())
    }
    /// Attempts count even when DNS/connect fails. Closing never refunds totals.
    pub fn open(&mut self) -> Result<(), EgressError> {
        if self.active >= self.max_active || self.total_connections >= self.caps.total_connections { return Err(EgressError::ResourceLimit); }
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
    #[test]
    fn limits_are_shared_and_not_refunded_by_close() {
        let now = Instant::now();
        let mut limits = default_limits();
        limits.max_tunnels = 1.try_into().unwrap();
        limits.bytes_per_second = 10.try_into().unwrap();
        limits.frames_per_second = 2.try_into().unwrap();
        let mut b = RunBudget::new(&limits, RunCaps { total_bytes: 12, total_connections: 2 }, now).unwrap();
        b.open().unwrap(); assert!(b.open().is_err()); b.close(); b.open().unwrap(); b.close(); assert!(b.open().is_err());
        b.bytes(10, now).unwrap(); assert!(b.bytes(1, now).is_err());
        b.bytes(2, now + Duration::from_secs(1)).unwrap(); assert!(b.bytes(1, now + Duration::from_secs(2)).is_err());
        b.frame(now).unwrap(); b.frame(now).unwrap(); assert!(b.frame(now).is_err());
        b.frame(now + Duration::from_millis(500)).unwrap();
        assert_eq!(b.totals(), (12,2));
    }
}
