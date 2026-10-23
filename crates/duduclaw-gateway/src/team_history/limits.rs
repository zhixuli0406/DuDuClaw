//! Per-home, per-viewer disclosure limits. No transcript data is retained.
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Default)]
struct Window {
    get: VecDeque<i64>,
    list: VecDeque<i64>,
}

impl Window {
    fn take(&mut self, get: bool, now: i64) -> Result<(), u64> {
        let queue = if get { &mut self.get } else { &mut self.list };
        while queue.front().is_some_and(|t| *t <= now - 3600) {
            queue.pop_front();
        }
        if queue.len() >= if get { 120 } else { 300 } {
            return Err((queue[0] + 3600 - now).max(1) as u64);
        }
        if get {
            let recent: Vec<_> = queue.iter().filter(|t| **t > now - 60).collect();
            if recent.len() >= 20 {
                return Err((*recent[0] + 60 - now).max(1) as u64);
            }
        }
        queue.push_back(now);
        Ok(())
    }
}

pub fn take(home: &Path, viewer: &str, get: bool) -> Result<(), u64> {
    static LIMITS: OnceLock<Mutex<HashMap<(PathBuf, String), Window>>> = OnceLock::new();
    let now = chrono::Utc::now().timestamp();
    let mut limits = LIMITS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| 60u64)?;
    limits.retain(|_, w| {
        w.get
            .back()
            .into_iter()
            .chain(w.list.back())
            .any(|t| *t > now - 3600)
    });
    if limits.len() >= 10_000 && !limits.contains_key(&(home.to_path_buf(), viewer.into())) {
        return Err(60);
    }
    limits
        .entry((home.to_path_buf(), viewer.into()))
        .or_default()
        .take(get, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn burst_hour_and_expiry_are_independent() {
        let mut w = Window::default();
        for _ in 0..20 {
            assert!(w.take(true, 100).is_ok())
        }
        assert_eq!(w.take(true, 100), Err(60));
        assert!(w.take(true, 160).is_ok());
        for _ in 0..300 {
            assert!(w.take(false, 160).is_ok())
        }
        assert_eq!(w.take(false, 160), Err(3600));
        assert!(w.take(false, 3760).is_ok());
        let mut w = Window::default();
        for minute in 0..6 {
            for _ in 0..20 {
                assert!(w.take(true, minute * 60).is_ok())
            }
        }
        assert_eq!(w.take(true, 400), Err(3200));
        assert!(w.take(true, 3600).is_ok());
    }
}
