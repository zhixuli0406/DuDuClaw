//! Native desktop control for DuDuClaw L5b Computer Use.
//!
//! Provides cross-platform mouse, keyboard, and screenshot capabilities
//! using `enigo` (input simulation) and `xcap` (screen capture).
//!
//! This crate powers the L5b (native) path, where the AI agent directly
//! controls the host machine's desktop — as opposed to L5a (container)
//! which uses Xvfb + xdotool inside a Docker container.
//!
//! # Security
//!
//! L5b is inherently higher risk than L5a. The caller is responsible for:
//! - Checking `CapabilitiesConfig.computer_use_mode == Native`
//! - Enforcing `CONTRACT.toml [must_not]` rules
//! - Running the `RiskDetector` before each action
//! - Providing channel-based confirmation for high-risk actions
//! - Implementing emergency stop (user activity detection via `rdev`)

pub mod activity_monitor;
pub mod controller;

pub use activity_monitor::ActivityMonitor;
pub use controller::{
    DesktopController, DesktopError, MouseButton, NativeDesktopController, ScrollDirection,
    parse_enigo_key,
};
