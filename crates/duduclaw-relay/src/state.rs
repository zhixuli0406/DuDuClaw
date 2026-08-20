//! Shared application state threaded through every route handler.

use std::sync::Arc;
use std::time::Duration;

use crate::db::RelayDb;
use crate::hook::HOOK_RATE_LIMIT_PER_MINUTE;
use crate::queue::OfflineQueue;
use crate::ratelimit::TokenBucketLimiter;
use crate::registry::ConnectionRegistry;

/// Requests/minute allowed per source IP on `/v1/device/register`. Registration
/// is otherwise unauthenticated (trust-on-first-use), so this is the only
/// guard against squatting/spam beyond the device_id format check.
const REGISTER_RATE_LIMIT_PER_MINUTE: u32 = 10;

/// Connection attempts/minute allowed per source IP on `/v1/device/ws`.
const WS_CONNECT_RATE_LIMIT_PER_MINUTE: u32 = 30;

pub struct AppState {
    pub db: RelayDb,
    pub registry: ConnectionRegistry,
    pub offline_queue: OfflineQueue,
    pub hook_rate_limiter: TokenBucketLimiter,
    pub register_rate_limiter: TokenBucketLimiter,
    pub ws_rate_limiter: TokenBucketLimiter,
}

impl AppState {
    pub fn new(db: RelayDb) -> Arc<Self> {
        Arc::new(Self {
            db,
            registry: ConnectionRegistry::new(),
            offline_queue: OfflineQueue::new(),
            hook_rate_limiter: TokenBucketLimiter::new(HOOK_RATE_LIMIT_PER_MINUTE, Duration::from_secs(60)),
            register_rate_limiter: TokenBucketLimiter::new(
                REGISTER_RATE_LIMIT_PER_MINUTE,
                Duration::from_secs(60),
            ),
            ws_rate_limiter: TokenBucketLimiter::new(
                WS_CONNECT_RATE_LIMIT_PER_MINUTE,
                Duration::from_secs(60),
            ),
        })
    }
}
