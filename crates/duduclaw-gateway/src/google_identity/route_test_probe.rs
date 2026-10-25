//! Test-only fail-safe at the token/effect work boundaries. Registered homes
//! refuse before credential/key/provider work, even if a negative route test
//! unexpectedly accepts a wire body. Tests require both counters to stay zero.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Copy)]
pub(crate) enum Work {
    Token,
    Effect,
}
fn registered() -> &'static Mutex<HashMap<PathBuf, [usize; 2]>> {
    static HOMES: OnceLock<Mutex<HashMap<PathBuf, [usize; 2]>>> = OnceLock::new();
    HOMES.get_or_init(Default::default)
}
pub(crate) struct Guard(PathBuf);
impl Guard {
    pub(crate) fn install(home: &Path) -> Self {
        let home = home.canonicalize().expect("owned route fixture home");
        assert!(
            registered()
                .lock()
                .unwrap()
                .insert(home.clone(), [0; 2])
                .is_none()
        );
        Self(home)
    }
    pub(crate) fn counts(&self) -> [usize; 2] {
        registered().lock().unwrap()[&self.0]
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        registered().lock().unwrap().remove(&self.0);
    }
}
pub(crate) fn refuse_work(home: &Path, work: Work) -> bool {
    let Ok(home) = home.canonicalize() else {
        return false;
    };
    let mut homes = registered().lock().unwrap();
    let Some(counts) = homes.get_mut(&home) else {
        return false;
    };
    counts[match work {
        Work::Token => 0,
        Work::Effect => 1,
    }] += 1;
    true
}
