//! D3-f2 (2026-08-23): where an **absolute** pointing device actually is,
//! read from the kernel rather than assumed.
//!
//! ## Why this module exists
//!
//! A Wayland compositor's pointer position is a value it maintains itself,
//! seeded by motion events. That works for a relative mouse — the first
//! nudge tells you everything. It does not work for an absolute device
//! (touchscreen, KVM, QEMU's `usb-tablet`): the kernel drops an `EV_ABS`
//! event whose value is unchanged (`input_handle_event`'s
//! `INPUT_IGNORE_EVENT` path), so a device that is *already* where the user
//! is about to press produces a button event and **no motion at all**. The
//! compositor then has a button with no idea where it happened, and its own
//! `PointerHandle` still sits at its `(0, 0)` default.
//!
//! D3-f closed half of that: [`crate::state::DuduclawComp::ensure_pointer_focus`]
//! synthesises an `enter` so the press has somewhere to go. This module
//! closes the other half — the `enter` now carries the **real** coordinates
//! instead of the origin.
//!
//! ## Where the number comes from
//!
//! `EVIOCGABS(ABS_X)` / `EVIOCGABS(ABS_Y)` on the device's evdev fd. The
//! kernel keeps the last reported value of every absolute axis per device
//! (that is exactly what it compares against to decide an event is
//! redundant), so this is the same number the next motion event would carry
//! — available at any moment, with no event required.
//!
//! ## Why we do not just `open("/dev/input/eventN")`
//!
//! On the appliance, comp runs as `duduclaw-kiosk` (uid 999, groups
//! `video`+`render`) while `/dev/input/event*` is `root:input 0660` —
//! measured on the VM, not assumed. A direct open is `EACCES`; input
//! devices reach comp only through **seatd**, which opens them privileged
//! and passes the fd over its socket. So instead of asking for a second,
//! wider permission, this module keeps a `dup()` of the fd libinput was
//! *already* given: [`RecordingInterface`] wraps whatever
//! `LibinputInterface` the backend hands to `Libinput::new_with_udev` and
//! records `(devnode, dup'd fd)` on every `open_restricted`, dropping the
//! entry again on `close_restricted`. No new privilege, no image change, no
//! second seatd round trip.
//!
//! Everything here degrades to `None`. A device we never saw opened, an
//! `ioctl` that fails, a degenerate axis range — each one just means "we
//! cannot improve on the compositor's own idea of the position", and the
//! caller falls back to exactly the pre-D3-f2 behaviour.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use smithay::reexports::input::LibinputInterface;
use smithay::utils::{Logical, Point, Rectangle};

/// `struct input_absinfo` is six `__s32`s: value, minimum, maximum, fuzz,
/// flat, resolution (`linux/input.h`). Its size is baked into the ioctl
/// request number, so it is a constant here rather than a `size_of` of some
/// local type that could drift away from the kernel's.
const ABSINFO_I32S: usize = 6;
const ABSINFO_BYTES: u32 = (ABSINFO_I32S * 4) as u32;

/// `EVIOCGABS(abs)` = `_IOR('E', 0x40 + abs, struct input_absinfo)`.
///
/// `_IOR(type, nr, size)` on Linux's `asm-generic/ioctl.h` is
/// `(_IOC_READ << 30) | (size << 16) | (type << 8) | nr`, with
/// `_IOC_READ == 2`. Spelled out rather than pulled from a bindgen crate:
/// this is two constants, and adding a dependency to compute them would be
/// more moving parts than the arithmetic it replaces.
const fn eviocgabs(axis: u32) -> u32 {
    (2 << 30) | (ABSINFO_BYTES << 16) | ((b'E' as u32) << 8) | (0x40 + axis)
}

const ABS_X: u32 = 0x00;
const ABS_Y: u32 = 0x01;

/// The three fields of `struct input_absinfo` this module needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
}

/// Where `info.value` sits in its axis range, as `0.0..=1.0`.
///
/// The denominator is `maximum - minimum + 1`, **not** `maximum - minimum`:
/// that is what libinput's own `scale_axis`/`absinfo_range` use, and this
/// number has to agree with the one the `PointerMotionAbsolute` arm gets
/// from `position_transformed` or a press and a subsequent motion at the
/// identical device value would land a fraction of a pixel apart.
///
/// `None` for a degenerate or out-of-range axis rather than a clamped
/// guess — a device that reports nonsense should fall back to the
/// compositor's own position, not be believed.
pub fn normalize(info: AbsInfo) -> Option<f64> {
    if info.maximum <= info.minimum {
        return None;
    }
    if info.value < info.minimum || info.value > info.maximum {
        return None;
    }
    let range = (info.maximum as f64) - (info.minimum as f64) + 1.0;
    Some(((info.value as f64) - (info.minimum as f64)) / range)
}

/// Place a normalised `(x, y)` onto an output's logical geometry.
///
/// Same composition as the `PointerMotionAbsolute` arm's
/// `position_transformed(output_geo.size) + output_geo.loc`, so a synthesised
/// position and a real motion event map identically.
pub fn map_to_output(nx: f64, ny: f64, geo: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    Point::from((
        geo.loc.x as f64 + nx * geo.size.w as f64,
        geo.loc.y as f64 + ny * geo.size.h as f64,
    ))
}

/// Live evdev fds, keyed by the raw fd number libinput was handed.
///
/// Cheap to clone (one `Arc`), so the backend can keep one and hand another
/// to [`crate::state::DuduclawComp`]. Empty by default, which is exactly what
/// the winit backend wants: every lookup misses and every caller falls back.
#[derive(Clone, Default)]
pub struct AbsPointerTable {
    // Keyed by the raw fd *value we returned to libinput* — that is the only
    // handle `close_restricted` gives us back, and it is unique for as long
    // as the fd is open, which is precisely the lifetime of the entry.
    inner: Arc<Mutex<HashMap<RawFd, (PathBuf, OwnedFd)>>>,
}

impl std::fmt::Debug for AbsPointerTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.lock().map(|t| t.len()).unwrap_or(0);
        f.debug_struct("AbsPointerTable").field("devices", &n).finish()
    }
}

impl AbsPointerTable {
    fn record(&self, path: &Path, key: RawFd, dup: OwnedFd) {
        if let Ok(mut t) = self.inner.lock() {
            t.insert(key, (path.to_path_buf(), dup));
        }
    }

    fn forget(&self, key: RawFd) {
        if let Ok(mut t) = self.inner.lock() {
            t.remove(&key);
        }
    }

    /// How many devices are currently tracked. Test/diagnostics only.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|t| t.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The normalised `(x, y)` of the device whose devnode basename is
    /// `sysname` (libinput's `Device::id()`, e.g. `"event1"`).
    ///
    /// Matched on `Path::file_name()` of a path libinput itself opened — the
    /// caller-supplied string is never used to *build* a path, so there is
    /// nothing here for a hostile device name to traverse into.
    pub fn normalized_position(&self, sysname: &str) -> Option<(f64, f64)> {
        if sysname.is_empty() {
            return None;
        }
        let guard = self.inner.lock().ok()?;
        let (_, fd) = guard
            .values()
            .find(|(path, _)| path.file_name().is_some_and(|n| n == sysname))?;
        let x = read_absinfo(fd.as_fd_borrowed(), ABS_X).and_then(normalize)?;
        let y = read_absinfo(fd.as_fd_borrowed(), ABS_Y).and_then(normalize)?;
        Some((x, y))
    }
}

/// Tiny local extension so the lookup above reads as one expression; `OwnedFd`
/// implements `AsFd`, this just names it without importing the trait at every
/// call site.
trait AsFdBorrowed {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_>;
}

impl AsFdBorrowed for OwnedFd {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.as_fd()
    }
}

fn read_absinfo(fd: BorrowedFd<'_>, axis: u32) -> Option<AbsInfo> {
    let mut buf = [0i32; ABSINFO_I32S];
    // SAFETY: `fd` is a live descriptor for an evdev character device (it
    // came from libinput's own `open_restricted`); `EVIOCGABS` is a pure
    // read of `ABSINFO_BYTES` into the caller's buffer, and `buf` is exactly
    // that many bytes of correctly-aligned `i32`. A device with no such axis
    // answers `EINVAL`, which is the `rc < 0` branch — measured on the VM's
    // `event0`/`event2` (Power Button / USB Keyboard), not assumed.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), eviocgabs(axis) as _, buf.as_mut_ptr()) };
    if rc < 0 {
        return None;
    }
    Some(AbsInfo {
        value: buf[0],
        minimum: buf[1],
        maximum: buf[2],
    })
}

/// A `LibinputInterface` that records the fds it opens on behalf of libinput.
///
/// Delegates every decision to `inner` — it opens nothing itself and can
/// therefore not widen what comp is allowed to touch. A failed `dup()` is
/// deliberately silent: it costs the position-lookup for that one device and
/// nothing else, and libinput's own open still succeeded.
pub struct RecordingInterface<I: LibinputInterface> {
    inner: I,
    table: AbsPointerTable,
}

impl<I: LibinputInterface> RecordingInterface<I> {
    pub fn new(inner: I) -> (Self, AbsPointerTable) {
        let table = AbsPointerTable::default();
        (
            Self {
                inner,
                table: table.clone(),
            },
            table,
        )
    }
}

impl<I: LibinputInterface> LibinputInterface for RecordingInterface<I> {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let fd = self.inner.open_restricted(path, flags)?;
        match fd.try_clone() {
            Ok(dup) => self.table.record(path, fd.as_raw_fd(), dup),
            Err(e) => tracing::debug!(
                path = %path.display(),
                error = %e,
                "abs_pointer: could not dup the evdev fd — absolute position lookups will fall back for this device"
            ),
        }
        Ok(fd)
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        self.table.forget(fd.as_raw_fd());
        self.inner.close_restricted(fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::Size;
    use std::os::fd::FromRawFd;

    #[test]
    fn eviocgabs_matches_the_kernel_macro() {
        // Cross-checked against a live `fcntl.ioctl` on the appliance VM's
        // `/dev/input/event1` (QEMU USB Tablet), which answered with a real
        // `input_absinfo` for exactly these two request numbers.
        assert_eq!(eviocgabs(ABS_X), 0x8018_4540);
        assert_eq!(eviocgabs(ABS_Y), 0x8018_4541);
    }

    #[test]
    fn normalize_uses_libinputs_inclusive_range() {
        // 0..=32767 is QEMU's tablet range; libinput divides by 32768.
        let info = AbsInfo {
            value: 16357,
            minimum: 0,
            maximum: 32767,
        };
        let n = normalize(info).expect("in-range value normalises");
        assert!((n - 16357.0 / 32768.0).abs() < 1e-12);
    }

    #[test]
    fn normalize_spans_zero_to_just_under_one() {
        let at_min = normalize(AbsInfo {
            value: 0,
            minimum: 0,
            maximum: 32767,
        })
        .unwrap();
        let at_max = normalize(AbsInfo {
            value: 32767,
            minimum: 0,
            maximum: 32767,
        })
        .unwrap();
        assert_eq!(at_min, 0.0);
        assert!(at_max < 1.0 && at_max > 0.99996);
    }

    #[test]
    fn normalize_handles_a_nonzero_minimum() {
        let n = normalize(AbsInfo {
            value: 50,
            minimum: -50,
            maximum: 50,
        })
        .unwrap();
        assert!((n - 100.0 / 101.0).abs() < 1e-12);
    }

    #[test]
    fn normalize_rejects_degenerate_and_out_of_range_axes() {
        // A device that reports no usable range (both the "axis absent"
        // all-zero shape and an inverted one).
        assert_eq!(
            normalize(AbsInfo {
                value: 0,
                minimum: 0,
                maximum: 0
            }),
            None
        );
        assert_eq!(
            normalize(AbsInfo {
                value: 0,
                minimum: 10,
                maximum: 5
            }),
            None
        );
        // A value outside its own declared range is not clamped — it is
        // disbelieved, so the caller keeps the compositor's own position.
        assert_eq!(
            normalize(AbsInfo {
                value: 40000,
                minimum: 0,
                maximum: 32767
            }),
            None
        );
        assert_eq!(
            normalize(AbsInfo {
                value: -1,
                minimum: 0,
                maximum: 32767
            }),
            None
        );
    }

    #[test]
    fn map_to_output_reproduces_the_measured_vm_coordinates() {
        // The exact numbers from the D3-f2 VM round: QMP put the tablet at
        // (639, 226) on a 1280x800 framebuffer, and EVIOCGABS read back
        // 16357 / 9256.
        let geo = Rectangle::new(Point::from((0, 0)), Size::from((1280, 800)));
        let nx = normalize(AbsInfo {
            value: 16357,
            minimum: 0,
            maximum: 32767,
        })
        .unwrap();
        let ny = normalize(AbsInfo {
            value: 9256,
            minimum: 0,
            maximum: 32767,
        })
        .unwrap();
        let p = map_to_output(nx, ny, geo);
        assert!((p.x - 639.0).abs() <= 2.0, "x was {}", p.x);
        assert!((p.y - 226.0).abs() <= 2.0, "y was {}", p.y);
    }

    #[test]
    fn map_to_output_honours_a_non_origin_output() {
        let geo = Rectangle::new(Point::from((100, 50)), Size::from((1280, 800)));
        let p = map_to_output(0.5, 0.25, geo);
        assert!((p.x - 740.0).abs() < 1e-9);
        assert!((p.y - 250.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_table_never_claims_to_know_a_position() {
        let t = AbsPointerTable::default();
        assert!(t.is_empty());
        assert_eq!(t.normalized_position("event1"), None);
        assert_eq!(t.normalized_position(""), None);
    }

    #[test]
    fn a_recorded_device_is_found_by_basename_and_dropped_on_close() {
        // Uses a pipe read end as a stand-in fd: the table's bookkeeping is
        // pure `HashMap` work and does not care what the fd points at (the
        // `EVIOCGABS` on it fails, which is the honest `None` below).
        //
        // Not `std::io::pipe()` (stabilized in rustc 1.87, this crate's
        // pinned container toolchain is 1.85.0 — see `BUILD.md`'s "Why
        // Docker" section) — `libc` is already a direct dependency (`Cargo.
        // toml`'s CD-2 entry), so a raw `libc::pipe` call needs no new one.
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        unsafe {
            libc::close(fds[1]);
        }
        let key = fd.as_raw_fd();
        let dup = fd.try_clone().expect("dup");
        let t = AbsPointerTable::default();
        t.record(Path::new("/dev/input/event7"), key, dup);
        assert_eq!(t.len(), 1);
        // Found by basename; the ioctl on a pipe fails, so the answer is a
        // truthful None rather than a fabricated coordinate.
        assert_eq!(t.normalized_position("event7"), None);
        // A name that is a *substring* of the real one must not match — the
        // lookup is whole-basename equality, per this repo's convention 2.
        assert_eq!(t.normalized_position("event"), None);
        assert_eq!(t.normalized_position("event77"), None);
        t.forget(key);
        assert!(t.is_empty());
    }
}
