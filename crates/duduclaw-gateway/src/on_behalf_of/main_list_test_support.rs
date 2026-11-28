//! Home-scoped list admission for tests, independent of the real execution gate.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn homes() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static HOMES: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    HOMES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[doc(hidden)]
#[must_use]
pub struct TestListInEffectGuard(PathBuf);

/// Enable only the main-person list for this home, including spawned work.
/// Nested guards keep the entry alive until the last guard is dropped.
#[doc(hidden)]
pub fn test_list_in_effect_for(home: &Path) -> TestListInEffectGuard {
    let home = home.canonicalize().expect("test home must exist");
    *homes().lock().expect("test list registry poisoned").entry(home.clone()).or_default() += 1;
    TestListInEffectGuard(home)
}

impl Drop for TestListInEffectGuard {
    fn drop(&mut self) {
        let mut homes = homes().lock().unwrap_or_else(|error| error.into_inner());
        if let Some(count) = homes.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 { homes.remove(&self.0); }
        }
    }
}

pub(super) fn enabled(home: &Path) -> bool {
    let Ok(home) = home.canonicalize() else { return false; };
    homes().lock().is_ok_and(|homes| homes.contains_key(&home))
}
