//! Package E: fd-only parser service. The gateway never enables `engine`.
pub mod client;
pub mod archive_adapter;
pub mod media_adapters;
pub mod policy;
pub mod response;
#[cfg(feature = "engine")]
pub mod parsers;
#[cfg(target_os = "linux")]
pub mod transport;
#[cfg(target_os = "linux")]
pub mod retirement;
#[cfg(target_os = "linux")]
mod retirement_hello;
#[cfg(all(target_os = "linux", feature = "engine"))]
pub mod sandbox;

pub use duduclaw_isolation::worker;

pub mod adapters;
#[cfg(feature = "engine")]
mod adapter_engine;
#[cfg(feature = "engine")]
#[path = "engine_knowledge/mod.rs"]
pub(crate) mod extract;

pub use duduclaw_isolation::bounded;
