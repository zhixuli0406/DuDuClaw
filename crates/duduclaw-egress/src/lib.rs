//! Public-IP-only TCP egress. Host authority never comes from data frames.
pub mod policy;
pub mod environment;
pub mod http;
pub mod budget;
#[cfg(target_os = "linux")]
pub mod session;
#[cfg(target_os = "linux")]
pub mod transport;
#[cfg(target_os = "linux")]
pub mod resolver;
#[cfg(target_os = "linux")]
pub mod daemon;
#[cfg(target_os = "linux")]
pub mod forwarder;

#[cfg(any(target_os = "linux", test))]
mod dns_wire;

#[cfg(target_os = "linux")]
pub mod seccomp;

#[cfg(target_os = "linux")]
pub mod controller;
