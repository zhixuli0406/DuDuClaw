//! `[approval] forbid_self_approval` (default off): a request raised while
//! an employee works for a person cannot be approved by that same person.
//! Denying stays possible. Bound requests are not covered here: they are
//! decided only by the exact person they are bound to.

use std::path::Path;

/// Reply when the decider is the person the request was raised for.
pub const REFUSAL: &str =
    "這項要求是 AI 員工替你辦事時提出的，依設定不能由你自己同意，請另一位有權限的人決定（拒絕仍可以）。";

/// `[approval] forbid_self_approval` from `<home>/config.toml`. Missing file
/// or key = off; a file that exists but cannot be read or parsed = on (a
/// gate that cannot read its config denies).
pub fn enabled(home: &Path) -> bool {
    let content = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return true;
    };
    let Some(section) = table.get("approval") else {
        return false;
    };
    let Some(section) = section.as_table() else {
        return true;
    };
    match section.get("forbid_self_approval") {
        None => false,
        Some(v) => v.as_bool().unwrap_or(true),
    }
}

/// Whether approving a request raised for `on_behalf_of` is refused for
/// `decider` (the decider's own principal, wire form).
pub fn refuses(home: &Path, on_behalf_of: Option<&str>, decider: Option<&str>, approve: bool) -> bool {
    if !approve {
        return false;
    }
    match (on_behalf_of, decider) {
        (Some(raised_for), Some(decider)) if raised_for == decider => enabled(home),
        _ => false,
    }
}
