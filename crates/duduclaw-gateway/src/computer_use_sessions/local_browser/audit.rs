//! Closed native-browser audit metadata. Callers must never pass page text,
//! keys, pixels, full URLs, tokens or the native pairing key.
use serde_json::{Value, json};
use std::path::Path;

pub(crate) fn record(home: &Path, agent: &str, event: &str, mut details: Value) {
    details["target"] = json!(super::super::SessionTarget::LocalBrowser);
    let entry = super::super::audit_entry(agent, event, details, None);
    tokio::spawn(super::super::write_audit(home.to_owned(), entry));
}
