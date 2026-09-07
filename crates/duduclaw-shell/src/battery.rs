// Local battery status — WP-fix-QEMU-a (2026-09-05), a QEMU walkthrough
// defect fix. The menu bar's "86%" (`fake_data::BATTERY_PCT`) was a literal
// constant, shown unconditionally even on an appliance with no battery
// hardware at all (the shipping target is a desktop-class appliance, not a
// laptop). This module reads the real thing instead — straight from Linux's
// sysfs power-supply tree, no gateway round trip involved.
//
// ── "Absent, not an error" ────────────────────────────────────────────────
// Same convention `apps::installed`'s own header comment establishes for
// its flatpak/XDG scan: a machine with no battery power-supply (the common
// case on the shipping appliance) or no `/sys/class/power_supply` tree at
// all (this crate's own macOS dev loop) is the CORRECT, quiet `None`
// answer — never a fabricated percentage, and never treated as a failure to
// log or retry. `home.rs::menu_bar_right` hides the battery reading
// entirely when this returns `None`, rather than showing "0%" or "--%".
//
// ── Cached, not a bare per-render syscall (2026-09-05 review fix) ────────
// The first version of this module read straight from sysfs on every call,
// on the reasoning that a `read_to_string` of a couple of already-known
// files is the same cost class as `chrono::Local::now()` — a call this
// crate already makes directly from render bodies. That reasoning was
// wrong about the calling frequency: `battery_percent()` is called from
// `home.rs::menu_bar_right`'s render body, and the MENU BAR re-renders on
// every `cx.notify()` of the shared `ShellView` — an overlay opening or
// closing, ANY feed refresh landing (approvals, in-progress tasks, running
// windows, installed apps…), not just this module's own clock-adjacent
// cadence. A `fs::read_dir` plus a `type`/`capacity` read per power-supply
// entry is real syscalls (opendir/readdir/open/read/close), and repeating
// that on every one of potentially many renders per second is not the same
// cost class as one in-process clock read at all.
//
// `BatteryCache` below memoizes the last reading for `CACHE_TTL` (30s — the
// same "how stale is too stale" cadence this crate's other gateway-poll
// feeds already use, e.g. `overlay::task_progress_feed::
// REFRESH_STALE_AFTER`), so the sysfs tree is actually walked at most once
// every 30s no matter how often the menu bar repaints in between. The
// "hidden when `None`" behavior is unchanged — a cached `None` stays hidden
// for the same 30s a cached `Some` stays shown, rather than re-probing on
// every render either way.

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";

/// How long a cached reading is trusted before `battery_percent()` touches
/// sysfs again — see this file's header comment for why this exists at all.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// The memoized "last reading" — `now`/`read` are passed into
/// [`BatteryCache::get_or_refresh`] rather than called from inside it, so
/// tests can drive both a fake clock and a fake reader without touching
/// real sysfs or sleeping a real 30s. Production code (`battery_percent`
/// below) owns exactly one instance of this behind a `static`; tests
/// construct their own so parallel test threads never share (and stomp on)
/// one global slot.
struct BatteryCache {
    last: Mutex<Option<(Instant, Option<u8>)>>,
}

impl BatteryCache {
    const fn new() -> Self {
        Self { last: Mutex::new(None) }
    }

    /// Returns the cached reading if it is younger than `CACHE_TTL` as of
    /// `now`; otherwise calls `read` exactly once, stamps the result with
    /// `now`, caches it, and returns it. A cached `None` (no battery found)
    /// is reused just like a cached `Some` — re-probing a battery-less
    /// machine every render would defeat the whole point of caching.
    fn get_or_refresh(&self, now: Instant, read: impl FnOnce() -> Option<u8>) -> Option<u8> {
        // A poisoned mutex here would mean a panic while holding the guard
        // below (a plain compare-and-maybe-write, nothing that itself
        // panics) — recovering it is strictly better than turning that into
        // a second panic on a render thread, same allowance `home.rs::png`'s
        // own cache mutex already takes.
        let mut guard = self.last.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((read_at, value)) = *guard {
            if now.saturating_duration_since(read_at) < CACHE_TTL {
                return value;
            }
        }
        let value = read();
        *guard = Some((now, value));
        value
    }
}

static CACHE: BatteryCache = BatteryCache::new();

/// The battery's charge percentage (0-100), or `None` when this machine has
/// no battery power-supply (a desktop appliance — see this file's header
/// comment) or the sysfs tree could not be read (non-Linux dev machine; a
/// battery driver that has not populated `capacity` yet). Cached for
/// [`CACHE_TTL`] — see this file's header comment for why a bare per-call
/// sysfs read was wrong for how often this is actually called.
pub fn battery_percent() -> Option<u8> {
    CACHE.get_or_refresh(Instant::now(), read_battery_percent_uncached)
}

/// The actual sysfs scan — unconditional, uncached; only ever called
/// through [`BatteryCache::get_or_refresh`] (production) or directly by
/// this module's own tests (to exercise the real scan logic without the
/// cache's timing getting in the way). Picks the first power-supply entry
/// whose own `type` file reads `Battery` — `Mains`/`USB` entries are
/// AC/charger inputs, not something with a charge level of their own, and
/// must never be misread as one.
fn read_battery_percent_uncached() -> Option<u8> {
    let entries = fs::read_dir(POWER_SUPPLY_ROOT).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if read_trimmed(&path.join("type")).as_deref() != Some("Battery") {
            continue;
        }
        let capacity = read_trimmed(&path.join("capacity"))?;
        if let Ok(pct) = capacity.parse::<u8>() {
            // `capacity` is documented 0-100, but nothing stops a driver bug
            // from reporting more — clamped rather than trusted verbatim,
            // since this renders directly as a UI percentage.
            return Some(pct.min(100));
        }
    }
    None
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_trimmed_strips_the_trailing_newline_sysfs_files_end_in() {
        let dir = std::env::temp_dir().join(format!("duduclaw-battery-test-{}-{}", std::process::id(), line!()));
        fs::create_dir_all(&dir).expect("test tempdir must be creatable");
        let file = dir.join("type");
        fs::write(&file, "Battery\n").expect("test file must be writable");
        assert_eq!(read_trimmed(&file), Some("Battery".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_trimmed_is_none_for_a_path_that_does_not_exist() {
        let dir = std::env::temp_dir().join(format!("duduclaw-battery-test-missing-{}-{}", std::process::id(), line!()));
        assert_eq!(read_trimmed(&dir.join("nope")), None);
    }

    #[test]
    fn battery_percent_is_honestly_none_when_this_machine_has_no_power_supply_tree() {
        // The load-bearing case this whole module exists for: this crate's
        // own dev loop runs on macOS, which has no `/sys` at all — the
        // correct answer is a quiet `None`, never a guessed percentage. Goes
        // through the real cached entry point on purpose (not
        // `read_battery_percent_uncached` directly): the cache must not
        // turn an honest `None` into anything else either.
        if !Path::new(POWER_SUPPLY_ROOT).exists() {
            assert_eq!(battery_percent(), None);
        }
    }

    // ── BatteryCache: the 2026-09-05 review fix ─────────────────────────
    // Each test builds its OWN `BatteryCache` (never the production
    // `static CACHE`) so parallel test threads never share one mutable
    // slot, and drives both the clock and the reader as plain closure
    // arguments — no real 30s sleep and no real sysfs touched.

    #[test]
    fn a_fresh_cache_calls_read_exactly_once_and_returns_its_value() {
        let cache = BatteryCache::new();
        let mut calls = 0;
        let value = cache.get_or_refresh(Instant::now(), || {
            calls += 1;
            Some(77)
        });
        assert_eq!(value, Some(77));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_reading_inside_the_ttl_is_reused_without_calling_read_again() {
        let cache = BatteryCache::new();
        let t0 = Instant::now();
        cache.get_or_refresh(t0, || Some(42));

        let mut calls = 0;
        let still_cached = cache.get_or_refresh(t0 + CACHE_TTL - Duration::from_secs(1), || {
            calls += 1;
            Some(99)
        });
        assert_eq!(still_cached, Some(42), "must reuse the cached reading inside the TTL, not the fresh 99");
        assert_eq!(calls, 0, "must not touch sysfs again while still within the TTL");
    }

    #[test]
    fn a_reading_at_or_past_the_ttl_triggers_exactly_one_fresh_read() {
        let cache = BatteryCache::new();
        let t0 = Instant::now();
        cache.get_or_refresh(t0, || Some(42));

        let mut calls = 0;
        let refreshed = cache.get_or_refresh(t0 + CACHE_TTL, || {
            calls += 1;
            Some(99)
        });
        assert_eq!(refreshed, Some(99), "must refresh once the TTL has fully elapsed");
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_cached_none_is_reused_just_like_a_cached_some() {
        // A battery-less machine's honest `None` must not be re-probed on
        // every render either — that would defeat the whole point of
        // caching for the single most common shipping case (a desktop
        // appliance with no battery at all).
        let cache = BatteryCache::new();
        let t0 = Instant::now();
        cache.get_or_refresh(t0, || None);

        let mut calls = 0;
        let still_none = cache.get_or_refresh(t0 + Duration::from_secs(5), || {
            calls += 1;
            Some(50)
        });
        assert_eq!(still_none, None);
        assert_eq!(calls, 0);
    }

    #[test]
    fn each_cache_instance_is_independent() {
        // Guards against a future refactor accidentally routing every
        // instance through one shared slot (e.g. a module-level `static`
        // hidden inside the method instead of `self.last`) — parallel tests
        // sharing state would then intermittently fail depending on
        // execution order.
        let a = BatteryCache::new();
        let b = BatteryCache::new();
        let t0 = Instant::now();
        a.get_or_refresh(t0, || Some(1));
        b.get_or_refresh(t0, || Some(2));
        assert_eq!(a.get_or_refresh(t0, || panic!("must not re-read within the TTL")), Some(1));
        assert_eq!(b.get_or_refresh(t0, || panic!("must not re-read within the TTL")), Some(2));
    }
}
