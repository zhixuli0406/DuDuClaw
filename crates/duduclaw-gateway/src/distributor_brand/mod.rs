//! Narrow signed distributor identity. No local setters, colours or About HTML.
pub mod document;
pub mod runtime;

use futures_util::FutureExt;
use serde::Serialize;

pub const ORIGINAL_PRODUCT_NAME: &str = "DuDuClaw";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Brand {
    pub product_name: String,
    pub logo_light: String,
    pub logo_dark: Option<String>,
}

/// Read the live snapshot without blocking synchronous channel renderers.
/// A busy license lock, missing runtime or mismatched subscription is closed.
pub fn effective() -> Option<Brand> {
    let snapshot = crate::license_runtime::global()?.snapshot().now_or_never()?;
    let accepted = runtime::current()?;
    effective_for(snapshot.tier, snapshot.subscription_id.as_deref(), &accepted)
}

fn effective_for(tier: duduclaw_license::LicenseTier, subscription: Option<&str>, accepted: &runtime::Accepted) -> Option<Brand> {
    (tier == duduclaw_license::LicenseTier::Oem && subscription == Some(accepted.subscription_id.as_str()))
        .then(|| accepted.brand.clone())
}

pub fn product_name() -> String {
    effective().map(|b| b.product_name).unwrap_or_else(|| ORIGINAL_PRODUCT_NAME.into())
}

/// Escape Markdown punctuation. HTML renderers escape the decoded literal once.
/// A zero-width separator prevents Discord mass mentions in plain messages.
pub fn escape_channel_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        match ch {
            '@' => out.push_str("@\u{200b}"),
            ch if ch.is_ascii_punctuation() => { out.push('\\'); out.push(ch); }
            _ => out.push(ch),
        }
    }
    out
}

pub fn channel_name() -> String { escape_channel_name(&product_name()) }

/// Content revision shared by authenticated metadata and the public response.
/// Includes the original-brand fallback, so clearing a brand changes revision.
pub fn revision(brand: Option<&Brand>) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(serde_json::to_vec(&brand).expect("brand serializes")))
}

#[cfg(test)]
mod tests;
