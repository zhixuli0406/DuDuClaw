//! Deterministic pause points for race tests (design §9.1 "卡點").
//!
//! In a non-test build every `pause_point` is an empty `async fn`. In tests a
//! case installs a gate for `(point, key)` — keys are unique ids, so parallel
//! tests never share a gate — and the code under test stops there until the
//! case releases it: the race lands at a fixed place instead of depending on
//! sleeps.

#[cfg(not(test))]
#[inline]
pub(crate) async fn pause_point(_point: &str, _key: &str) {}

#[cfg(test)]
pub(crate) use imp::*;

#[cfg(test)]
mod imp {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};

    use tokio::sync::Barrier;

    /// A one-shot gate: the code under test waits on `arrived`, then on
    /// `release`; the test waits on `arrived` (now the code is parked), acts,
    /// then waits on `release` to let it continue.
    pub(crate) struct Gate {
        pub arrived: Barrier,
        pub release: Barrier,
    }

    static GATES: LazyLock<Mutex<HashMap<(String, String), Arc<Gate>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) fn install(point: &str, key: &str) -> Arc<Gate> {
        let gate = Arc::new(Gate {
            arrived: Barrier::new(2),
            release: Barrier::new(2),
        });
        GATES
            .lock()
            .unwrap()
            .insert((point.to_string(), key.to_string()), Arc::clone(&gate));
        gate
    }

    pub(crate) async fn pause_point(point: &str, key: &str) {
        let gate = GATES
            .lock()
            .unwrap()
            .remove(&(point.to_string(), key.to_string()));
        if let Some(gate) = gate {
            gate.arrived.wait().await;
            gate.release.wait().await;
        }
    }
}
