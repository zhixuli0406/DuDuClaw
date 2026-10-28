//! A dedicated local browser. No desktop capture, OS input synthesis, TCP CDP,
//! user-supplied browser/profile paths, or remote dashboard approvals.

pub(crate) mod audit;
#[cfg(target_os = "macos")]
pub(crate) mod backend;
#[cfg(unix)]
pub(crate) mod cdp_pipe;
pub(crate) mod companion;
pub(crate) mod consent;
pub(crate) mod native_start;
pub(crate) mod doctor;
pub(crate) mod launch;
#[cfg(unix)]
pub(crate) mod machine_lease;
#[cfg(all(test, target_os = "macos"))]
pub(super) mod native_fixture;
#[cfg(unix)]
pub(crate) mod page_guard;
pub(crate) mod policy;
pub(crate) mod presence;
pub(crate) mod review_details;
#[cfg(target_os = "macos")]
pub(crate) mod processes;
#[cfg(unix)]
pub(crate) mod profile;
pub(crate) mod proxy;
