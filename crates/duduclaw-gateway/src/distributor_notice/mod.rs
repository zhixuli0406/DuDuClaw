//! Optional distributor-information block for the dashboard About page.
//!
//! The block is a build input of the distributor build
//! (`DUDUCLAW_DISTRIBUTOR_NOTICE_PATH`, read by `build.rs`). Public builds
//! compile `None`, so `about.get` and the About page stay unchanged. There is
//! no RPC, setting or file that can change it at runtime: end customers cannot
//! replace it with their own content.
//!
//! The compiled HTML was sanitized at build time and is sanitized again here
//! before it is served (defence in depth); the dashboard renders it only in a
//! script-less sandboxed iframe.

pub mod sanitize;

#[cfg(test)]
mod build_input;
#[cfg(test)]
mod tests;

use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/distributor_notice.rs"));

static RUNTIME_NOTICE: OnceLock<Option<String>> = OnceLock::new();

/// The distributor notice compiled into this binary, re-sanitized once at
/// first use. `None` for public builds.
pub fn distributor_notice() -> Option<String> {
    RUNTIME_NOTICE
        .get_or_init(|| resanitize(COMPILED_NOTICE))
        .clone()
}

/// True when this binary was built with a distributor notice.
pub fn is_compiled_in() -> bool {
    COMPILED_NOTICE.is_some()
}

fn resanitize(compiled: Option<&str>) -> Option<String> {
    let raw = compiled?;
    match sanitize::sanitize_notice_html(raw) {
        Ok(html) => Some(html),
        Err(error) => {
            // Fail closed: a notice that no longer passes is not shown.
            tracing::warn!(%error, "compiled distributor notice refused at runtime");
            None
        }
    }
}
