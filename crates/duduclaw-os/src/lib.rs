//! OS environment integration for DuDuClaw — Phase 1 of the "OS-native agent"
//! track.
//!
//! This crate turns the gateway from a passive message responder into an agent
//! that perceives its host OS. It is deliberately lightweight: filesystem events
//! come from the cross-platform `notify` stack, and every macOS-specific
//! capability (notifications, `open`) **shells out** (`osascript` / `open`)
//! exactly like `duduclaw-desktop`'s screen capture — no `objc`/`cocoa` bindings,
//! so there are no code-signing / linkage headaches.
//!
//! Everything here is deny-by-default from the caller's perspective: the crate
//! provides mechanism only. Capability gating (`[capabilities] os_native`),
//! scope checks, and the ActionGuard irreversibility gate live in the MCP
//! dispatch layer (`duduclaw-cli`), which is the single complete-mediation point.
//!
//! Modules:
//! - [`watch`]        — [`watch::OsWatcher`]: debounced, rate-limited FS events.
//! - [`notify_native`]— [`notify_native::send_notification`]: native desktop toast.
//! - [`open_target`]  — [`open_target::open_path_or_url`]: open a file / http(s) URL.
//! - [`frontmost`]    — [`frontmost::frontmost_info`]: foreground app + window title (P2-4).
//! - [`spotlight`]    — [`spotlight::spotlight_search`]: `mdfind` metadata search (P2-4).
//! - [`calendar`]     — [`calendar::today_events`]: read-only today's calendar events via JXA (P2-4).

pub mod calendar;
pub mod frontmost;
pub mod notify_native;
pub mod open_target;
pub mod spotlight;
pub mod watch;

pub use calendar::{CalendarError, CalendarEvent, today_events};
pub use frontmost::{FrontmostError, FrontmostInfo, frontmost_info};
pub use notify_native::{NotifyError, send_notification};
pub use open_target::{OpenError, OpenTarget, classify_target, open_path_or_url};
pub use spotlight::{SpotlightError, spotlight_search};
pub use watch::{
    FileEventKind, OsFileEvent, OsWatcher, WatchConfig, WatchError, WatchHandle, WatchStats,
};
