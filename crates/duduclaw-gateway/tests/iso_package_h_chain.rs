//! H15 (v1.81 Package H): live acceptance of the 29 non-connector tools on a
//! disposable root Linux host with the INSTALLED protected service, real
//! spawnd and the operator's tool driver. Acceptance matrix additions 16–27:
//!
//! | # | check | ids |
//! |---|---|---|
//! | 16 | host facts (kernel, Landlock ABI, distro) + spawnd feature bits | H16-* |
//! | 17 | file_read / csv_read / xlsx_read through the run-bound work broker | H17-* |
//! | 18 | broker refusals: traversal, absolute path, symlink, `.forks` | H18-* |
//! | 19 | web_fetch_cached / web_extract on a public fixture origin (PublicOrigins) | H19-* |
//! | 20 | redirect, DNS-rebinding / private address, IP literal, http refused | H20-* |
//! | 21 | execute_program / code_map / office_script in-run helpers, synchronous | H21-* |
//! | 22 | CLI tool timeout projection ≥ 305 s; helper timeout and cancel keep the run | H22-* |
//! | 23 | skill_hub_install from the local fixture hub (own SKILLS only) | H23-* |
//! | 24 | hostile hub archives refused: zip bomb, traversal, symlink | H24-* |
//! | 25 | spawn_agent / spawn_ephemeral; D8 = one capability run per employee | H25-* |
//! | 26 | recording gate: `helper_recording_unsupported` until the probe passed | H26-* |
//! | 27 | readiness reasons agree with the feature bits | H27-* |
//!
//! ```sh
//! sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_STATE_DIR=/root/iso-state DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
//!   [DUDUCLAW_ISO_PUBLIC_ORIGIN=https://fixture.example.org DUDUCLAW_ISO_REBIND_URL=https://rebind.example.org/ok] \
//!   [DUDUCLAW_ISO_HUB_FIXTURE=1] \
//!   cargo test -p duduclaw-gateway --test iso_package_h_chain -- --ignored iso_root_h15_ --test-threads=1 --nocapture
//! ```
//! Skips cleanly (prints `SKIP <test> <reason>`) when not root / not
//! disposable / no spawnd / no admin session. Missing driver, fixture origin
//! or fixture hub ⇒ per-check `SKIP` lines naming what is missing.
#![cfg(target_os = "linux")]
mod iso_chain_support;
use iso_chain_support::*;
use serde_json::{json, Value};
use std::{fs, path::Path, time::{Duration, Instant}};

const WORK: &str = "employee-work";

#[test]
#[ignore = "internal: executed by iso_root_* as the service account"]
fn chain_token_child() { token_child_main(); }

/// One Shared employee with the production-shaped profile: wildcard
/// `mcp__duduclaw__*` allowlist and the admin-only `[isolation] public_web`.
fn employee(env: &Env, report: &mut Report, label: &str) -> Option<String> {
    let node = match create_shared(env, label) {
        Ok(node) => node,
        Err(error) => { report.fail("H15-setup", format!("{label}: {error}")); return None; }
    };
    let path = env.data.join("agents").join(label).join("agent.toml");
    let edited = fs::read_to_string(&path).ok().and_then(|raw| raw.parse::<toml::Table>().ok()).and_then(|mut doc| {
        let caps = doc.entry("capabilities").or_insert(toml::Value::Table(Default::default())).as_table_mut()?;
        caps.insert("allowed_tools".into(), toml::Value::Array(vec![toml::Value::String("mcp__duduclaw__*".into())]));
        let isolation = doc.entry("isolation").or_insert(toml::Value::Table(Default::default())).as_table_mut()?;
        isolation.insert("public_web".into(), toml::Value::Boolean(true));
        let permissions = doc.entry("permissions").or_insert(toml::Value::Table(Default::default())).as_table_mut()?;
        permissions.insert("can_create_agents".into(), toml::Value::Boolean(true));
        // Overwrite in place: owner and mode of the existing file are kept.
        fs::write(&path, toml::to_string(&doc).ok()?).ok()
    }).is_some();
    report.check("H15-setup", edited, format!("{label}: wildcard allowlist + [isolation] public_web = true (admin config)"));
    employee_hex(&node)
}

fn call(env: &Env, label: &str, tool: &str, args: Value) -> Option<Value> { env.tool(label, tool, args) }
fn expect(report: &mut Report, id: &str, reply: Option<Value>, want: &str, what: &str) -> Option<Value> {
    match reply {
        None => { report.skip(id, format!("{what}: needs DUDUCLAW_ISO_TOOL_DRIVER")); None }
        Some(reply) => {
            report.check(id, outcome(&reply) == want, format!("{what}: outcome={} code={} (want {want})", outcome(&reply), code(&reply)));
            Some(reply)
        }
    }
}

// ── 16 / 27: host facts, features, readiness ─────────────────────────────
fn landlock_abi() -> i64 {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<u8>(), 0usize, 1u32) }
}

#[test]
#[ignore = "iso_root_: installed protected service on a disposable root Linux VM; host facts + readiness"]
fn iso_root_h15_host_facts_features_and_readiness() {
    let test = "iso_root_h15_host_facts_features_and_readiness";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let os = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let pretty = os.lines().find_map(|line| line.strip_prefix("PRETTY_NAME=")).unwrap_or("?").trim_matches('"').to_owned();
    let abi = landlock_abi();
    eprintln!("FACT kernel={} distro={pretty} landlock_abi={abi}", kernel.trim());
    report.check("H16-landlock", abi >= 1, format!("Landlock ABI {abi} (22.04/Debian 12 lack TCP pinning below ABI 4: T5 residual)"));
    let systemd = std::process::Command::new("systemctl").arg("--version").output().ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_owned()).unwrap_or_default();
    eprintln!("FACT systemd={systemd}");
    match duduclaw_spawnd::host::EvidenceReport::read(env.service.1) {
        Ok(evidence) => {
            eprintln!("FACT spawnd_features={} work_broker={} helper_calls={} helper_recording={} landlock_selected={:?} recording_probe={}",
                serde_json::to_string(&evidence.features).unwrap_or_default(), evidence.work_broker(), evidence.helper_calls(),
                evidence.helper_recording(), evidence.landlock.as_ref().map(|l| l.selected_abi),
                evidence.recording_probe.is_some());
            report.check("H16-broker", evidence.work_broker(), "spawnd advertises work_broker");
            if evidence.helper_calls() { report.pass("H16-helpers", "spawnd advertises helper_calls"); }
            else { report.skip("H16-helpers", "spawnd does not advertise helper_calls (no helper runtime published / CVE-2021-4197 probe failed): H21/H22 will refuse"); }
            report.check("H16-recording", !evidence.helper_recording() && !duduclaw_spawnd::host::EvidenceReport::read(env.service.1)
                .map(|e| e.recording_probe_passed()).unwrap_or(false),
                "helper_recording is off (build kill switch RECORDING_BUILD_ENABLED = false)");
            let status = env.admin("system.status", json!({}));
            let text = status.to_string();
            report.check("H27-broker", evidence.work_broker() || text.contains("workspace_broker_unavailable"),
                "readiness reports workspace_broker_unavailable exactly when work_broker is missing");
            if !evidence.helper_calls() {
                report.check("H27-helpers", text.contains("helper_launcher_unavailable") || text.contains("helper_runtime_missing"),
                    "readiness names the missing helper launcher/runtime");
            }
            if text.contains("duduclaw-browser-record-v1") || text.contains("helper_recording_unsupported") {
                report.check("H27-recording", text.contains("helper_recording_unsupported"), "readiness: helper_recording_unsupported");
            } else { report.skip("H27-recording", "no recording runtime published: nothing to report"); }
        }
        Err(error) => report.fail("H16-features", format!("spawnd readiness.json unreadable: {error}")),
    }
    report.check("H22-projection", duduclaw_mcp_boundary::config::CLI_TOOL_TIMEOUT_MS >= 305_000,
        format!("CLI tool timeout projection {} ms ≥ 300 s helper bound + 5 s", duduclaw_mcp_boundary::config::CLI_TOOL_TIMEOUT_MS));
    report.finish();
}

// ── 17 / 18: brokered file tools ─────────────────────────────────────────
const XLSX_PY: &str = r#"import zipfile,os
os.makedirs('/work/h15',exist_ok=True)
z=zipfile.ZipFile('/work/h15/book.xlsx','w')
z.writestr('[Content_Types].xml','<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>')
z.writestr('_rels/.rels','<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>')
z.writestr('xl/workbook.xml','<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S1" sheetId="1" r:id="rId1"/></sheets></workbook>')
z.writestr('xl/_rels/workbook.xml.rels','<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>')
z.writestr('xl/worksheets/sheet1.xml','<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>k</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>h15xlsx</t></is></c></row></sheetData></worksheet>')
z.close()
open('/work/h15/note.txt','w').write('h15-note\n')
open('/work/h15/table.csv','w').write('k,v\nh15csv,1\n')
try: os.symlink('/etc/passwd','/work/h15/link.txt')
except FileExistsError: pass
print('seeded')"#;

#[test]
#[ignore = "iso_root_: installed protected service + tool driver on a disposable root Linux VM; file tools"]
fn iso_root_h15_file_tools_via_run_bound_broker() {
    let test = "iso_root_h15_file_tools_via_run_bound_broker";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let label = format!("h15f-{}", env.nonce);
    if employee(&env, &mut report, &label).is_none() { return report.finish(); }
    // Seed /work in one run (the work tree persists across the employee's runs).
    let seeded = env.driver(&["seed", &label]).filter(|reply| outcome(reply) == "ok")
        .or_else(|| call(&env, &label, "execute_program", json!({"language": "python", "code": XLSX_PY, "workspace": WORK})));
    let Some(seeded) = seeded else { report.skip("H17", "needs DUDUCLAW_ISO_TOOL_DRIVER (seed or execute_program)"); return report.finish(); };
    if outcome(&seeded) != "ok" || !contains_text(&seeded, "seeded") {
        report.fail("H17-seed", format!("could not seed /work/h15 (outcome={} code={})", outcome(&seeded), code(&seeded)));
        return report.finish();
    }
    let read = expect(&mut report, "H17-file", call(&env, &label, "file_read", json!({"workspace": WORK, "path": "h15/note.txt"})),
        "ok", "file_read through the run-bound broker");
    if let Some(read) = read {
        report.check("H17-file-body", contains_text(&read, "h15-note"), "file_read returned the seeded bytes");
        let text = read.to_string();
        report.check("H17-no-host-path", !text.contains("/var/lib/duduclaw") && !text.contains("/work/"),
            "result carries the employee-relative path only, never a host path");
    }
    if let Some(reply) = expect(&mut report, "H17-csv", call(&env, &label, "csv_read", json!({"workspace": WORK, "path": "h15/table.csv"})),
        "ok", "csv_read parsed by the parse worker") {
        report.check("H17-csv-body", contains_text(&reply, "h15csv"), "csv rows parsed");
    }
    if let Some(reply) = expect(&mut report, "H17-xlsx", call(&env, &label, "xlsx_read", json!({"workspace": WORK, "path": "h15/book.xlsx"})),
        "ok", "xlsx_read parsed by the parse worker") {
        report.check("H17-xlsx-body", contains_text(&reply, "h15xlsx"), "xlsx rows parsed");
    }
    for (id, path) in [("H18-traversal", "../../etc/passwd"), ("H18-absolute", "/etc/passwd"), ("H18-symlink", "h15/link.txt"),
        ("H18-forks", ".forks/x/note.txt"), ("H18-missing", "h15/absent.txt")] {
        if let Some(reply) = expect(&mut report, id, call(&env, &label, "file_read", json!({"workspace": WORK, "path": path})),
            "refused", &format!("file_read {path:?}")) {
            report.check(&format!("{id}-leak"), !contains_text(&reply, "root:x:0:0"), "no host file content in the refusal");
        }
    }
    report.finish();
}

// ── 19 / 20: PublicOrigins web tools ─────────────────────────────────────
#[test]
#[ignore = "iso_root_: installed protected service + tool driver + public fixture origin; web tools"]
fn iso_root_h15_web_public_origins_and_refusals() {
    let test = "iso_root_h15_web_public_origins_and_refusals";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let label = format!("h15w-{}", env.nonce);
    if employee(&env, &mut report, &label).is_none() { return report.finish(); }
    match std::env::var("DUDUCLAW_ISO_PUBLIC_ORIGIN").ok().filter(|origin| origin.starts_with("https://")) {
        None => report.skip("H19", "needs DUDUCLAW_ISO_PUBLIC_ORIGIN (https origin with a public DNS name and address serving fixtures/public_origin.py)"),
        Some(origin) => {
            let origin = origin.trim_end_matches('/');
            if let Some(reply) = expect(&mut report, "H19-fetch", call(&env, &label, "web_fetch_cached",
                json!({"url": format!("{origin}/h15/ok")})), "ok", "web_fetch_cached on the public fixture origin") {
                report.check("H19-fetch-body", contains_text(&reply, "h15-public-ok"), "fixture body returned");
            }
            if let Some(reply) = expect(&mut report, "H19-extract", call(&env, &label, "web_extract",
                json!({"url": format!("{origin}/h15/page.html"), "selector": "#h15", "format": "text"})), "ok", "web_extract via ExtractHtml") {
                report.check("H19-extract-body", contains_text(&reply, "h15-extract-target"), "selector extracted");
            }
            expect(&mut report, "H20-redirect", call(&env, &label, "web_fetch_cached",
                json!({"url": format!("{origin}/h15/redirect")})), "refused", "302 to a private address is not followed");
            expect(&mut report, "H20-http", call(&env, &label, "web_fetch_cached",
                json!({"url": origin.replacen("https://", "http://", 1) + "/h15/ok"})), "refused", "http is outside the HTTPS-only grant");
        }
    }
    match std::env::var("DUDUCLAW_ISO_REBIND_URL").ok() {
        None => report.skip("H20-rebind", "needs DUDUCLAW_ISO_REBIND_URL (a public name that resolves to a private / loopback address)"),
        Some(url) => { expect(&mut report, "H20-rebind", call(&env, &label, "web_fetch_cached", json!({"url": url})),
            "refused", "DNS answer with a private address refused before connect"); }
    }
    for (id, url) in [("H20-ip", "https://10.0.0.1/"), ("H20-loopback", "https://localhost/"),
        ("H20-userinfo", "https://user:pw@example.com/"), ("H20-metadata", "https://169.254.169.254/latest/meta-data/")] {
        expect(&mut report, id, call(&env, &label, "web_fetch_cached", json!({"url": url})), "refused", &format!("{url} refused"));
    }
    report.finish();
}

// ── 21 / 22: in-run helpers ──────────────────────────────────────────────
#[test]
#[ignore = "iso_root_: installed protected service + tool driver + signed helper runtimes; in-run helpers"]
fn iso_root_h15_in_run_helpers_timeout_and_cancel() {
    let test = "iso_root_h15_in_run_helpers_timeout_and_cancel";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let label = format!("h15x-{}", env.nonce);
    if employee(&env, &mut report, &label).is_none() { return report.finish(); }
    if let Some(reply) = expect(&mut report, "H21-program", call(&env, &label, "execute_program",
        json!({"language": "python", "code": "print(6*7)", "workspace": WORK})), "ok", "execute_program returns synchronously") {
        report.check("H21-program-out", contains_text(&reply, "42"), "helper stdout returned in the same call");
        let effective = field(&reply, &["effective_timeout_ms", "result.effective_timeout_ms"]).and_then(|v| v.parse::<u64>().ok());
        report.check("H22-effective", effective.is_some_and(|ms| ms > 0 && ms <= 300_000),
            format!("effective_timeout_ms {effective:?} within (0, 300 s]"));
    }
    let offline = expect(&mut report, "H21-offline", call(&env, &label, "execute_program", json!({"language": "python",
        "code": "import socket\ntry:\n socket.create_connection(('1.1.1.1',443),3); print('connected')\nexcept Exception as e: print('blocked', type(e).__name__)",
        "workspace": WORK})), "ok", "offline helper");
    if let Some(reply) = offline {
        report.check("H21-offline-net", !contains_text(&reply, "connected") && contains_text(&reply, "blocked"),
            "offline helper cannot open a socket (seccomp EPERM)");
    }
    expect(&mut report, "H21-codemap", call(&env, &label, "code_map", json!({"query": "h15", "workspace": WORK})),
        "ok", "code_map in-run helper");
    expect(&mut report, "H21-office", call(&env, &label, "office_script",
        json!({"skill": "xlsx", "script": "extract", "args": "[\"h15/book.xlsx\"]", "workspace": WORK})), "ok", "office_script in-run helper");
    // Timeout: the helper is killed, the run survives and answers the next call.
    let started = Instant::now();
    expect(&mut report, "H22-timeout", call(&env, &label, "execute_program", json!({"language": "python",
        "code": "import time\ntime.sleep(30)", "timeout_seconds": 5, "workspace": WORK})), "refused", "helper timeout ⇒ helper_timed_out");
    report.check("H22-timeout-bound", started.elapsed() < Duration::from_secs(60), format!("timeout honoured in {:?}", started.elapsed()));
    // Cancel (driver contract `cancel`): MCP notifications/cancelled kills the helper only.
    match env.driver(&["cancel", &label, "execute_program",
        &json!({"language": "bash", "code": "sleep 120", "workspace": WORK}).to_string(), "3000"]) {
        None => report.skip("H22-cancel", "needs DUDUCLAW_ISO_TOOL_DRIVER `cancel`"),
        Some(reply) => {
            report.check("H22-cancel", outcome(&reply) == "cancelled", format!("cancel outcome={}", outcome(&reply)));
            report.check("H22-cancel-run", field(&reply, &["run_alive_after"]).as_deref() == Some("true"),
                "the run survived the cancel (HelperCancel never kills the run)");
            report.check("H22-cancel-next", field(&reply, &["next_call"]).as_deref() == Some("ok"),
                "the same run serves the next helper call");
        }
    }
    report.finish();
}

// ── 23 / 24: skill hub fixture ───────────────────────────────────────────
#[test]
#[ignore = "iso_root_: installed protected service + tool driver + fixture hub override; skill_hub_install"]
fn iso_root_h15_skill_hub_fixture_and_hostile_archives() {
    let test = "iso_root_h15_skill_hub_fixture_and_hostile_archives";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    if std::env::var("DUDUCLAW_ISO_HUB_FIXTURE").as_deref() != Ok("1") {
        report.skip("H23", "needs DUDUCLAW_ISO_HUB_FIXTURE=1: fixture hub serving clawhub.ai on a public address with a trusted test CA");
        return report.finish();
    }
    let label = format!("h15s-{}", env.nonce);
    if employee(&env, &mut report, &label).is_none() { return report.finish(); }
    let skills = env.data.join("agents").join(&label).join("SKILLS");
    if expect(&mut report, "H23-install", call(&env, &label, "skill_hub_install",
        json!({"hub": "clawhub", "skill_name": "h15-good"})), "ok", "fixture skill installed").is_some() {
        report.check("H23-file", skills.join("h15-good.md").is_file(), "agents/<label>/SKILLS/h15-good.md written");
    }
    for (id, slug) in [("H24-bomb", "h15-bomb"), ("H24-traversal", "h15-traversal"), ("H24-symlink", "h15-symlink"),
        ("H24-flagged", "h15-flagged")] {
        expect(&mut report, id, call(&env, &label, "skill_hub_install", json!({"hub": "clawhub", "skill_name": slug})),
            "refused", &format!("hostile archive {slug} refused"));
        report.check(&format!("{id}-nofile"), !skills.join(format!("{slug}.md")).exists(), "nothing installed");
    }
    let escaped = ["/tmp/h15-escape", "/var/lib/duduclaw/h15-escape"].iter().any(|path| Path::new(path).exists())
        || env.data.join("h15-escape").exists();
    report.check("H24-contained", !escaped, "no traversal entry landed outside the extraction");
    report.finish();
}

// ── 25: spawn tools under D8 ─────────────────────────────────────────────
fn pid_namespaces(uid: u32) -> std::collections::BTreeSet<String> {
    uid_processes(uid).into_iter().filter_map(|pid| fs::read_link(format!("/proc/{pid}/ns/pid")).ok())
        .map(|link| link.to_string_lossy().into_owned()).collect()
}

#[test]
#[ignore = "iso_root_: installed protected service + tool driver; spawn tools and D8"]
fn iso_root_h15_spawn_tools_one_run_per_employee() {
    let test = "iso_root_h15_spawn_tools_one_run_per_employee";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let (source, target) = (format!("h15a-{}", env.nonce), format!("h15b-{}", env.nonce));
    let Some(hex) = employee(&env, &mut report, &source) else { return report.finish() };
    if employee(&env, &mut report, &target).is_none() { return report.finish(); }
    let Some((uid, _)) = registry_account(&hex) else { report.fail("H25-setup", "no registry account"); return report.finish() };
    // Sample the employee's isolated runs while the calls are in flight.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = { let stop = stop.clone(); std::thread::spawn(move || {
        let mut max = 0usize;
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            max = max.max(pid_namespaces(uid).len());
            std::thread::sleep(Duration::from_millis(50));
        }
        max
    }) };
    expect(&mut report, "H25-agent", call(&env, &source, "spawn_agent", json!({"agent_id": target, "task": "h15 delegated task"})),
        "ok", "spawn_agent queued to another Shared employee");
    expect(&mut report, "H25-ephemeral", call(&env, &source, "spawn_ephemeral",
        json!({"instruction": "reply h15", "context": "h15", "tools": ["memory_search"], "tier": "cheap"})),
        "ok", "spawn_ephemeral queued under the parent identity");
    expect(&mut report, "H25-forged", call(&env, &source, "spawn_agent",
        json!({"agent_id": target, "task": "x", "sender_agent": "other", "delegation_depth": 0})), "refused",
        "forged delegation metadata refused by the exact schema");
    // The child may start only after the parent run ended (D8): keep sampling.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !uid_processes(uid).is_empty() { std::thread::sleep(Duration::from_millis(200)); }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let max = sampler.join().unwrap_or(usize::MAX);
    report.check("H25-d8", max <= 1, format!("at most one isolated run (pid namespace) of the employee at any sample: max {max}"));
    report.finish();
}

// ── 26: recording gate ───────────────────────────────────────────────────
#[test]
#[ignore = "iso_root_: installed protected service + tool driver; recording gate"]
fn iso_root_h15_recording_gate_refuses_until_probe_passed() {
    let test = "iso_root_h15_recording_gate_refuses_until_probe_passed";
    let Some(env) = env_or_skip(test) else { return };
    let mut report = Report::new(test);
    let probe_passed = duduclaw_spawnd::host::EvidenceReport::read(env.service.1).map(|e| e.helper_recording()).unwrap_or(false);
    if probe_passed {
        report.skip("H26", "helper_recording is advertised on this host (probe passed and the kill switch is open): the refusal path is not reachable");
        return report.finish();
    }
    let label = format!("h15r-{}", env.nonce);
    if employee(&env, &mut report, &label).is_none() { return report.finish(); }
    let url = std::env::var("DUDUCLAW_ISO_PUBLIC_ORIGIN").unwrap_or_else(|_| "https://example.com".into());
    if let Some(reply) = call(&env, &label, "browser_record_start", json!({"url": url, "workspace": WORK})) {
        // Hidden (not signed) or refused with the named code; never started.
        let refused = outcome(&reply) == "refused";
        let named = code(&reply) == "helper_recording_unsupported" || contains_text(&reply, "helper_recording_unsupported")
            || contains_text(&reply, "unknown tool") || contains_text(&reply, "not available");
        report.check("H26-start", refused && named, format!("browser_record_start refused (code={})", code(&reply)));
        report.check("H26-no-job", !contains_text(&reply, "\"recording\""), "no recording job was created");
    } else { report.skip("H26-start", "needs DUDUCLAW_ISO_TOOL_DRIVER"); }
    let audit = fs::read_to_string(env.data.join("security_audit.jsonl")).unwrap_or_default();
    report.check("H26-audit", !audit.contains("isolation_recording_started"), "no isolation_recording_started audit event");
    report.finish();
}
