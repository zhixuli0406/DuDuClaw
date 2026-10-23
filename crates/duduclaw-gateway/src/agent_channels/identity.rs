//! Which bot account answers under a channel label (`telegram:<agent>` →
//! `@sales_bot`), learned from the platform when the bot connects. Read by
//! the per-employee channel status; never holds a credential.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn map() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// Record the account name (bot username, service-account email) for `label`.
pub fn set(label: &str, account: &str) {
    let account = account.trim();
    if account.is_empty() {
        return;
    }
    map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(label.to_string(), account.to_string());
}

/// The account last recorded for `label`.
pub fn get(label: &str) -> Option<String> {
    map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(label)
        .cloned()
}

/// Forget `label` (its bot was removed or replaced).
pub fn clear(label: &str) {
    map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(label);
}
