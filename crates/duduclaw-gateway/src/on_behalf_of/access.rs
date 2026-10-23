//! `[delegation] human_principal = "enforce"`: a delegation the employee
//! tree allowed must also be something the person it is for may do.
//!
//! | person | `send_to_agent` / `spawn_agent` (Call) | task hand-over (Assign) |
//! |---|---|---|
//! | Admin account | yes | yes |
//! | other active account | direct binding or team grant (any level), or a `public_trigger` employee | direct binding or team grant at Operator or above |
//! | unmapped sender | `public_trigger` employees only | never |
//! | unknown / inactive account, unreadable `users.db` | no | no |
//!
//! No principal (cron, heartbeat, an employee's own work) adds no check.
//! A principal value that is present but malformed denies in enforce mode
//! and is only recorded in record mode. This check only ever narrows: it
//! runs after the employee tree allowed the delegation.

use std::path::Path;

use duduclaw_core::{
    DelegationDenied, DelegationDenyReason, DelegationPolicy, HumanPrincipal, HumanPrincipalMode,
    HumanRight, PrincipalRejection,
};

/// May `principal` have work of kind `right` handed to `target`?
/// `Err` when the identity store cannot be read (callers deny).
pub fn principal_permits(
    home: &Path,
    principal: &HumanPrincipal,
    target: &str,
    right: HumanRight,
) -> Result<bool, String> {
    match principal {
        HumanPrincipal::Unmapped { .. } => Ok(right == HumanRight::Call && is_public_trigger(home, target)),
        HumanPrincipal::User(user_id) => {
            let Some(db) = crate::decision_notify::open_user_db(home)? else {
                // No identity system at all: nobody can be named, so nothing
                // a named person asks for can be proven.
                return Ok(false);
            };
            // The same live computation `@` routing uses (team grants only
            // on the Enterprise edition).
            let Some(access) = crate::trigger_route::access::load_account(home, &db, user_id)? else {
                return Ok(false);
            };
            Ok(match right {
                HumanRight::Assign => access.may_assign(target),
                HumanRight::Call => access.may_call(target) || is_public_trigger(home, target),
            })
        }
    }
}

/// `[channels] public_trigger = true` in `<home>/agents/<target>/agent.toml`.
/// Unreadable or malformed counts as not public.
pub fn is_public_trigger(home: &Path, target: &str) -> bool {
    if !duduclaw_core::is_valid_agent_id(target) {
        return false;
    }
    let path = home.join("agents").join(target).join("agent.toml");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| c.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("channels")
                .and_then(|c| c.as_table())
                .and_then(|c| c.get("public_trigger"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false)
}

/// The enforce-mode check for one delegation `sender → target` on
/// `path_kind`, after the employee tree allowed it. `principal` is what the
/// carrier held: absent, valid, or rejected. Record mode never refuses (a
/// rejected value is audited by the caller that read it).
pub fn check(
    home: &Path,
    mode: HumanPrincipalMode,
    principal: &Result<Option<HumanPrincipal>, PrincipalRejection>,
    sender: &str,
    target: &str,
    path_kind: &str,
    policy: DelegationPolicy,
) -> Result<(), DelegationDenied> {
    if mode == HumanPrincipalMode::Record {
        return Ok(());
    }
    let deny = || DelegationDenied {
        sender: sender.trim().to_string(),
        target: target.trim().to_string(),
        policy,
        reason: DelegationDenyReason::HumanPrincipalNotPermitted,
    };
    let principal = match principal {
        Ok(None) => return Ok(()),
        Ok(Some(p)) => p,
        Err(_) => return Err(deny()),
    };
    let right = duduclaw_core::human_right_for_path(path_kind);
    match principal_permits(home, principal, target.trim(), right) {
        Ok(true) => Ok(()),
        Ok(false) => Err(deny()),
        Err(e) => {
            tracing::warn!(error = %e, "on_behalf_of: identity store unreadable; delegation denied");
            Err(deny())
        }
    }
}
