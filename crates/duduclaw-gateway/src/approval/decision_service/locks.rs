/// Per-approval-id lock serializing side effect + decision across transports.
/// Entries are weak and removed once no request holds them.
pub(crate) fn decision_lock(id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    let map = LOCKS.get_or_init(Default::default);
    let mut m = map.lock().unwrap_or_else(|p| p.into_inner());
    m.retain(|_, w| w.strong_count() > 0);
    if let Some(l) = m.get(id).and_then(std::sync::Weak::upgrade) {
        return l;
    }
    let l = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    m.insert(id.to_string(), std::sync::Arc::downgrade(&l));
    l
}

#[cfg(test)]
static FORCED_DECIDE_FAILURE: std::sync::Mutex<Option<(String, String)>> =
    std::sync::Mutex::new(None);

/// Test seam: make the next `decide` for `id` fail with `err`.
#[cfg(test)]
pub(crate) fn force_decide_failure(id: &str, err: &str) {
    *FORCED_DECIDE_FAILURE.lock().unwrap() = Some((id.to_string(), err.to_string()));
}

#[cfg(test)]
pub(super) fn take_forced_decide_failure(id: &str) -> Option<String> {
    let mut g = FORCED_DECIDE_FAILURE.lock().unwrap();
    match g.as_ref() {
        Some((i, _)) if i == id => g.take().map(|(_, e)| e),
        _ => None,
    }
}

