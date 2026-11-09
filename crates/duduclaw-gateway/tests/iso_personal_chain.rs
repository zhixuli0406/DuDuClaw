//! G2 [SEC] (v1.81): Personal / Shared employee lifecycle end to end on a
//! disposable root Linux host with the INSTALLED protected service and the
//! real spawnd (`/run/duduclaw-spawnd/{control,allocation}.sock`,
//! `/var/lib/duduclaw-spawnd/registry.json`).
//!
//! Personal chain: staff request → Admin proposal (step-up) → approval card →
//! root allocation → a run of the owner's employee with the owner's own
//! (Personal) database connector → tombstone (stops the live run, old token
//! dead) → restore under the SAME employee id and uid (incarnation + 1, token
//! rotated) → spawnd state reset → adoption under a NEW employee id and uid
//! (old token refused) → purge + root retire frees the label (a new request
//! gets a new id and uid). Shared: tombstone → restore with the member set.
//!
//! ```sh
//! sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_STATE_DIR=/root/iso-state \
//!   [DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver] [DUDUCLAW_ISO_G2_ALLOW_SPAWND_RESET=1] \
//!   cargo test -p duduclaw-gateway --test iso_personal_chain -- --ignored iso_root_e2e_ --test-threads=1 --nocapture
//! ```
//! (`package-h-acceptance.sh` prepares the host, the admin state and runs
//! the prebuilt test binary.) Without root / a disposable flag / spawnd / an
//! admin session the tests print `SKIP … <reason>` and pass. Steps that need
//! something the host cannot provide (tool driver, a qualifying second Admin
//! for the F6 §3.4 approval rule, permission to reset spawnd) are `SKIP`
//! lines with the reason; an RPC this build does not implement is a `FAIL`.
#![cfg(target_os = "linux")]
mod iso_chain_support;
use iso_chain_support::*;
use serde_json::{json, Value};
use std::process::Command;

const TOKEN_CHILD_TEST: &str = "chain_token_child";
/// Approval refusals the F6 §3.4 rule gives on a host whose Admin log is
/// younger than 7 days or that has a single Admin: not a product failure.
const APPROVAL_RULE_CODES: [&str; 3] = ["APPROVER_NOT_QUALIFIED", "ADMIN_LOG_UNAVAILABLE", "SELF_APPROVAL_COOLING"];

/// Gateway-uid child of `token_probe` (never selected by the `iso_root_` filter).
#[test]
#[ignore = "internal: executed by iso_root_e2e_* as the service account"]
fn chain_token_child() { token_child_main(); }

fn approver(env: &Env) -> &'static str {
    if env.state_file("admin2").is_file() { "admin2" } else { "admin" }
}
fn approval_rule_refusal(reply: &Value) -> bool {
    let text = error_text(reply);
    APPROVAL_RULE_CODES.iter().any(|code| text.contains(code))
}
/// `isolation.employee_restore_decide approve` by the approver; `Ok(false)`
/// when the §3.4 rule refuses on this host (recorded as SKIP by the caller).
fn decide(env: &Env, request: &str) -> Result<bool, String> {
    let who = approver(env);
    let reply = env.call_as(who, "isolation.employee_restore_decide",
        json!({"request_id": request, "decision": "approve", "csrf_token": "@csrf"}), true);
    if ok(&reply) { return Ok(true); }
    if approval_rule_refusal(&reply) { return Ok(false); }
    Err(error_text(&reply))
}
fn request_id(reply: &Value) -> Option<String> {
    field(reply, &["payload.request_id", "payload.id", "payload.request.request_id", "payload.request.id"])
}

/// A sqlite source + a Personal database route of `owner` pinned to `label`.
fn owner_connector(env: &Env, report: &mut Report, owner_id: &str, label: &str) -> Option<String> {
    let source = format!("g2db{}", env.nonce);
    let path = env.data.join("fixtures").join(format!("{source}.db"));
    let (uid, gid) = (env.service.0.to_string(), env.service.1.to_string());
    let created = Command::new("setpriv").args(["--reuid", uid.as_str(), "--regid", gid.as_str(), "--clear-groups",
        "python3", "-I", "-c", "import os,sqlite3,sys;os.makedirs(os.path.dirname(sys.argv[1]),0o700,exist_ok=True);\
db=sqlite3.connect(sys.argv[1]);db.execute('CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY,name TEXT)');\
db.execute(\"INSERT OR REPLACE INTO t VALUES(1,'alpha')\");db.commit()"]).arg(&path).status();
    if !created.is_ok_and(|status| status.success()) { report.fail("G2-03", "could not create the sqlite fixture as the service account"); return None; }
    let reply = env.admin("db_sources.upsert", json!({"name": source, "driver": "sqlite", "url": path,
        "allowed_tables": ["t"], "skip_test": true}));
    if !ok(&reply) { report.fail("G2-03", format!("db_sources.upsert: {}", error_text(&reply))); return None; }
    let reply = env.admin("db_sources.grants.set", json!({"name": source, "agents": [label]}));
    if !ok(&reply) { report.fail("G2-03", format!("db_sources.grants.set: {}", error_text(&reply))); return None; }
    let reply = env.admin_stepup("isolation.connector_provision", json!({"provider": "database", "kind": "personal",
        "binding": "personal", "label": format!("{label}-db"), "owner_user_id": owner_id,
        "details": {"employee": label, "source": source}}));
    report.check("G2-03", ok(&reply), format!("Personal database route of the owner provisioned for {label} ({})",
        if ok(&reply) { "ok".to_owned() } else { error_text(&reply) }));
    ok(&reply).then_some(source)
}

fn tombstone(env: &Env, report: &mut Report, id: &str, label: &str, hex: &str) -> Option<Value> {
    let reply = env.admin_stepup("isolation.employee_tombstone", json!({"label": label, "employee_id": hex}));
    if !ok(&reply) { report.fail(id, format!("tombstone {label} refused: {}", error_text(&reply))); return None; }
    let unconfirmed = contains_text(&reply, "stop_unconfirmed");
    report.check(&format!("{id}-fences"), !unconfirmed,
        if unconfirmed { "tombstone recorded runs:stop_unconfirmed (spawnd StopEmployeeRuns not confirmed)".to_owned() }
        else { "tombstone confirmed every run stopped (no runs:stop_unconfirmed fence)".to_owned() });
    report.check(&format!("{id}-dead"), live_employee(env, label).is_none(), format!("{label} no longer live after tombstone"));
    Some(reply)
}

#[test]
#[ignore = "iso_root_: installed protected service on a disposable root Linux VM (DUDUCLAW_ISO_DISPOSABLE=1); destructive"]
fn iso_root_e2e_personal_chain_tombstone_restore_adopt_purge_retire() {
    let test = "iso_root_e2e_personal_chain_tombstone_restore_adopt_purge_retire";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let label = format!("g2p-{}", env.nonce);
    let owner_id = match create_user(&env, "g2owner") {
        Ok(id) => id,
        Err(error) => { report.fail("G2-00", error); return report.finish(); }
    };
    let tokens = token_dir(&env);
    let old_token = tokens.join("incarnation0");

    // 1-2. proposal → approval → root allocation.
    let first = match create_personal(&env, "g2owner", &label) {
        Ok(node) => node,
        Err(error) => { report.fail("G2-01", format!("Personal employee not created: {error}")); return report.finish(); }
    };
    let Some(hex) = employee_hex(&first) else { report.fail("G2-01", "employee_list gives no employee id"); return report.finish(); };
    report.check("G2-01", contains_text(&first, &owner_id) && contains_text(&first, "ersonal"),
        format!("{label} confirmed as Personal(owner) with employee id {hex}"));
    let Some((uid, state)) = registry_account(&hex) else {
        report.fail("G2-02", "no spawnd registry account for the confirmed employee"); return report.finish();
    };
    report.check("G2-02", state == "ready", format!("root allocation: uid {uid}, registry state {state}"));
    let token = token_probe(&env, TOKEN_CHILD_TEST, "capture", &label, &old_token);
    report.check("G2-02t", token["captured"] == true && token["v2"] == true, format!("v2 agent token of incarnation 0: {token}"));

    // 3. a run with the owner's connector.
    let source = owner_connector(&env, &mut report, &owner_id, &label);
    match (&source, env.tool(&label, "db_select", json!({"source": source, "table": "t"}))) {
        (Some(_), Some(reply)) => report.check("G2-03r", outcome(&reply) == "ok",
            format!("owner's run reads through its Personal route (outcome={} code={})", outcome(&reply), code(&reply))),
        (_, None) => report.skip("G2-03r", "live run with the owner's connector: needs DUDUCLAW_ISO_TOOL_DRIVER"),
        (None, Some(_)) => report.skip("G2-03r", "no connector route (see G2-03)"),
    }

    // 4. tombstone stops a live run.
    let handle = env.driver(&["capture", &label, "g2owner"]).and_then(|reply| field(&reply, &["handle"]));
    let live_before = uid_processes(uid);
    if handle.is_none() { report.skip("G2-04a", "no live run held open: needs DUDUCLAW_ISO_TOOL_DRIVER capture"); }
    else { report.check("G2-04a", !live_before.is_empty(), format!("live run before tombstone: {} employee-uid processes", live_before.len())); }
    if tombstone(&env, &mut report, "G2-04", &label, &hex).is_none() { return report.finish(); }
    match wait_no_processes(uid, 10) {
        Ok(ms) => report.pass("G2-04b", format!("no process of uid {uid} {ms} ms after the tombstone")),
        Err(pids) => report.fail("G2-04b", format!("uid {uid} still runs {pids:?} 10 s after the tombstone")),
    }
    if let Some(handle) = &handle {
        let reply = env.driver(&["replay", handle, "db_select", &json!({"source": source, "table": "t"}).to_string()]);
        report.check("G2-04c", reply.as_ref().is_some_and(|reply| outcome(reply) == "refused"),
            "the pre-tombstone capability is refused");
    }
    let probe = token_probe(&env, TOKEN_CHILD_TEST, "verify", &label, &old_token);
    report.check("G2-04t", probe["verifies"] == false && probe["current_exists"] == false,
        format!("tombstoned employee: old token refused, no current token ({probe})"));

    // 5. restore the same employee id and uid.
    let reply = env.call_as("g2owner", "me.isolation.request_restore", json!({"label": label, "employee_id": hex}), true);
    let restored = if unimplemented(&reply) {
        report.fail("G2-05", "me.isolation.request_restore is not implemented in this build (F6c)"); false
    } else if let Some(request) = request_id(&reply) {
        match decide(&env, &request) {
            Ok(true) => true,
            Ok(false) => { report.skip("G2-05", "restore approval refused by the F6 §3.4 rule on this host \
                (needs a second Admin eligible ≥ 7 days: admin2.json, or an aged admin_eligibility_log)"); false }
            Err(error) => { report.fail("G2-05", format!("restore decide: {error}")); false }
        }
    } else { report.fail("G2-05", format!("restore request refused: {}", error_text(&reply))); false };
    let mut current_hex = hex.clone();
    let mut uids = vec![uid];
    if restored {
        match wait_live(&env, &label, 120) {
            Some(node) => {
                report.check("G2-05a", employee_hex(&node).as_deref() == Some(hex.as_str()), "restored under the same employee id");
                report.check("G2-05b", incarnation(&node) == Some(1), format!("incarnation {:?} (expected 1)", incarnation(&node)));
                report.check("G2-05c", registry_account(&hex).map(|(u, _)| u) == Some(uid), format!("same root uid {uid}"));
                let probe = token_probe(&env, TOKEN_CHILD_TEST, "verify", &label, &old_token);
                report.check("G2-05t", probe["verifies"] == false && probe["current_exists"] == true && probe["current_differs"] == true,
                    format!("restore rotated the token: incarnation-0 token refused ({probe})"));
            }
            None => report.fail("G2-05a", "approved restore did not make the employee live within 120 s"),
        }
        let _ = token_probe(&env, TOKEN_CHILD_TEST, "capture", &label, &old_token);
    }

    // 6. adoption after a spawnd state reset: new id + uid, old token refused.
    if std::env::var("DUDUCLAW_ISO_G2_ALLOW_SPAWND_RESET").as_deref() != Ok("1") {
        report.skip("G2-06", "adoption needs a spawnd state reset: set DUDUCLAW_ISO_G2_ALLOW_SPAWND_RESET=1 \
            (moves /var/lib/duduclaw-spawnd aside; every employee on the host loses its root allocation)");
    } else if !restored {
        report.skip("G2-06", "adoption runs after a restored employee (G2-05 did not restore)");
    } else if tombstone(&env, &mut report, "G2-06", &label, &hex).is_some() {
        let backup = format!("/var/lib/duduclaw-spawnd.g2-reset-{}", env.nonce);
        let script = format!("systemctl stop duduclaw-spawnd.service duduclaw-spawnd.socket \
            && mv /var/lib/duduclaw-spawnd {backup} && systemctl start duduclaw-spawnd.socket; systemctl start duduclaw-spawnd.service");
        let reset = Command::new("sh").args(["-c", script.as_str()]).status().is_ok_and(|status| status.success());
        // The gateway reconnects to the fresh spawnd; give readiness a moment.
        std::thread::sleep(std::time::Duration::from_secs(5));
        report.check("G2-06r", reset, format!("spawnd state moved to {backup} and spawnd restarted"));
        let listing = env.call_as("g2owner", "me.isolation.list_restorable", json!({}), false);
        report.check("G2-06g", contains_text(&listing, "ROOT_GONE") || contains_text(&listing, "root_gone"),
            "owner sees ROOT_GONE (authoritative NotFound) for the tombstoned employee");
        let reply = env.call_as("g2owner", "me.isolation.request_adoption", json!({"label": label, "employee_id": hex}), true);
        if unimplemented(&reply) { report.fail("G2-06a", "me.isolation.request_adoption is not implemented in this build (F6g)"); }
        else if let Some(request) = request_id(&reply) {
            let propose = env.admin_stepup("isolation.employee_propose",
                json!({"label": label, "binding_kind": "personal", "request_id": request}));
            if !ok(&propose) { report.fail("G2-06a", format!("adoption proposal: {}", error_text(&propose))); }
            else if let Err(error) = approve_proposal(&env, &label) {
                if error.contains("APPROVER") || error.contains("ADMIN_LOG") { report.skip("G2-06a", format!("approval rule: {error}")); }
                else { report.fail("G2-06a", error); }
            } else if let Some(node) = wait_live(&env, &label, 120) {
                let new_hex = employee_hex(&node).unwrap_or_default();
                let new_uid = registry_account(&new_hex).map(|(u, _)| u);
                report.check("G2-06a", !new_hex.is_empty() && new_hex != hex, format!("adopted under a new employee id {new_hex}"));
                report.check("G2-06b", new_uid.is_some_and(|u| !uids.contains(&u)), format!("new root uid {new_uid:?} (old {uids:?})"));
                let probe = token_probe(&env, TOKEN_CHILD_TEST, "verify", &label, &old_token);
                report.check("G2-06t", probe["verifies"] == false, format!("pre-adoption token refused ({probe})"));
                if let Some(u) = new_uid { uids.push(u); }
                current_hex = new_hex;
            } else { report.fail("G2-06a", "adopted employee not live within 120 s"); }
        } else { report.fail("G2-06a", format!("adoption request refused: {}", error_text(&reply))); }
    }

    // 7. purge + retire frees the label: a new request gets a new id and uid.
    let holder = live_employee(&env, &label).and_then(|node| employee_hex(&node));
    let target = holder.clone().unwrap_or_else(|| current_hex.clone());
    if holder.is_some() && tombstone(&env, &mut report, "G2-07", &label, &target).is_none() { return report.finish(); }
    let purge = env.admin_stepup("isolation.quarantine_purge",
        json!({"label": label, "employee_id": target, "confirm_early": true, "retire": true}));
    let purged = if unimplemented(&purge) {
        report.fail("G2-07p", "isolation.quarantine_purge is not implemented in this build (F6 §3.8)"); false
    } else if !ok(&purge) {
        report.fail("G2-07p", format!("purge refused: {}", error_text(&purge))); false
    } else if let Some(request) = request_id(&purge) {
        match decide(&env, &request) {
            Ok(true) => true,
            Ok(false) => { report.skip("G2-07p", "early purge (< 90 days) approval refused by the §3.4 rule on this host \
                (needs a second Admin eligible ≥ 7 days)"); false }
            Err(error) => { report.fail("G2-07p", format!("early purge decide: {error}")); false }
        }
    } else { true };
    if purged {
        report.pass("G2-07p", "quarantine purged");
        let staged = env.admin_stepup("isolation.employee_retire_propose", json!({"label": label, "employee_id": target}));
        if unimplemented(&staged) { report.fail("G2-07r", "isolation.employee_retire_propose is not implemented (F6l)"); }
        else {
            // `retire: true` may already have staged it; either way approve the open card.
            match approve_proposal(&env, &label) {
                Ok(()) => {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                    let mut gone = false;
                    while std::time::Instant::now() < deadline {
                        if registry_account(&target).is_none_or(|(_, state)| state != "ready") { gone = true; break; }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    report.check("G2-07r", gone, "root retire: the old employee's spawnd account is no longer Ready");
                }
                Err(error) => report.fail("G2-07r", format!("retire proposal approval: {error}")),
            }
            match create_personal(&env, "g2owner", &label) {
                Ok(node) => {
                    let new_hex = employee_hex(&node).unwrap_or_default();
                    let new_uid = registry_account(&new_hex).map(|(u, _)| u);
                    report.check("G2-07a", !new_hex.is_empty() && new_hex != hex && new_hex != target,
                        format!("freed label re-requested: new employee id {new_hex}"));
                    report.check("G2-07b", new_uid.is_some_and(|u| !uids.contains(&u)), format!("new uid {new_uid:?} (earlier {uids:?})"));
                }
                Err(error) => report.fail("G2-07a", format!("label not free after purge + retire: {error}")),
            }
        }
    }
    report.finish();
}

#[test]
#[ignore = "iso_root_: installed protected service on a disposable root Linux VM (DUDUCLAW_ISO_DISPOSABLE=1); destructive"]
fn iso_root_e2e_shared_tombstone_restore() {
    let test = "iso_root_e2e_shared_tombstone_restore";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let label = format!("g2s-{}", env.nonce);
    let node = match create_shared(&env, &label) {
        Ok(node) => node,
        Err(error) => { report.fail("G2S-01", error); return report.finish(); }
    };
    let Some(hex) = employee_hex(&node) else { report.fail("G2S-01", "no employee id"); return report.finish(); };
    report.check("G2S-01", contains_text(&node, "hared"), format!("{label} confirmed as Shared ({hex})"));
    let uid = registry_account(&hex).map(|(u, _)| u);
    let member = match create_user(&env, "g2member") {
        Ok(id) => id,
        Err(error) => { report.fail("G2S-02", error); return report.finish(); }
    };
    let bound = env.admin("users.bind_agent", json!({"user_id": member, "agent_name": label, "access_level": "operator"}));
    report.check("G2S-02", ok(&bound), format!("member bound as operator ({})", if ok(&bound) { "ok".into() } else { error_text(&bound) }));

    let reply = env.admin_stepup("isolation.employee_tombstone", json!({"label": label, "employee_id": hex}));
    if !ok(&reply) {
        let text = error_text(&reply);
        report.fail("G2S-03", if text.contains("NOT_PERSONAL") { "Shared tombstone refused NOT_PERSONAL (F6d not in this build)".into() }
            else { format!("Shared tombstone refused: {text}") });
        return report.finish();
    }
    report.check("G2S-03", live_employee(&env, &label).is_none(), "Shared employee tombstoned");
    let after = env.admin("users.list_agent_bindings", json!({"agent_name": label}));
    if ok(&after) { report.check("G2S-03b", !contains_text(&after, &member), "member bindings removed at tombstone"); }
    else { report.skip("G2S-03b", format!("member bindings not readable over RPC: {}", error_text(&after))); }
    if let Some(uid) = uid {
        match wait_no_processes(uid, 10) {
            Ok(ms) => report.pass("G2S-03c", format!("no process of uid {uid} ({ms} ms)")),
            Err(pids) => report.fail("G2S-03c", format!("uid {uid} still runs {pids:?}")),
        }
    }

    let reply = env.admin_stepup("isolation.shared_restore_request", json!({"label": label, "employee_id": hex,
        "members": [{"user_id": member, "access_level": "operator"}], "routes": []}));
    if unimplemented(&reply) { report.fail("G2S-04", "isolation.shared_restore_request is not implemented in this build (F6 §3.9)"); return report.finish(); }
    let Some(request) = request_id(&reply) else { report.fail("G2S-04", format!("restore request refused: {}", error_text(&reply))); return report.finish(); };
    match decide(&env, &request) {
        Ok(true) => match wait_live(&env, &label, 120) {
            Some(node) => {
                report.check("G2S-04a", employee_hex(&node).as_deref() == Some(hex.as_str()), "Shared restore keeps the employee id");
                report.check("G2S-04b", incarnation(&node) == Some(1), format!("incarnation {:?}", incarnation(&node)));
                report.check("G2S-04c", registry_account(&hex).map(|(u, _)| u) == uid, format!("same root uid {uid:?}"));
                let members = env.admin("users.list_agent_bindings", json!({"agent_name": label}));
                if ok(&members) { report.check("G2S-04d", contains_text(&members, &member), "selected member re-bound at its level"); }
                else { report.skip("G2S-04d", format!("member bindings not readable over RPC: {}", error_text(&members))); }
            }
            None => report.fail("G2S-04a", "approved Shared restore not live within 120 s"),
        },
        Ok(false) => report.skip("G2S-04", "Shared restore approval refused by the §3.4 rule on this host (second qualifying Admin needed)"),
        Err(error) => report.fail("G2S-04", format!("decide: {error}")),
    }
    report.finish();
}
