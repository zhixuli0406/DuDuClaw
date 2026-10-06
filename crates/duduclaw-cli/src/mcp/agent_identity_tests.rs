//! Verify `get_default_agent`'s preference order:
//! `DUDUCLAW_AGENT_ID` env > `config.toml [general] default_agent` > `"dudu"`.
//!
//! The env var is process-wide, so these tests must run serially.
//! A `Mutex` guards the env-mutation scope; we hold the guard across
//! the whole test, including the async `get_default_agent` call.

use super::get_default_agent;
use std::fs;
use std::sync::Mutex;

/// Serializes any test that reads/writes `DUDUCLAW_AGENT_ID`.
/// Without this, parallel tests corrupt each other's env view.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Minimal `TempDir` copy (the outer `tests` module already has one,
/// but it's not accessible from a sibling module).
struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir()
            .join(format!("duduclaw-agent-identity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_default_agent_config(home: &std::path::Path, default_agent: &str) {
    let content = format!("[general]\ndefault_agent = \"{default_agent}\"\n");
    fs::write(home.join("config.toml"), content).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn agent_id_env_overrides_config_default() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_default_agent_config(tmp.path(), "agnes");

    // SAFETY: env mutation serialized via ENV_LOCK for this test module.
    unsafe {
        std::env::set_var(duduclaw_core::ENV_AGENT_ID, "duduclaw-tl");
    }
    let result = get_default_agent(tmp.path()).await;
    unsafe {
        std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
    }

    assert_eq!(
        result, "duduclaw-tl",
        "env var must override config.toml default_agent"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_id_env_missing_falls_back_to_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_default_agent_config(tmp.path(), "agnes");

    // Make sure no stray env from other tests interferes.
    unsafe {
        std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
    }

    let result = get_default_agent(tmp.path()).await;
    assert_eq!(result, "agnes", "missing env → fall back to config");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_id_env_empty_string_falls_back_to_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_default_agent_config(tmp.path(), "agnes");

    unsafe {
        std::env::set_var(duduclaw_core::ENV_AGENT_ID, "");
    }
    let result = get_default_agent(tmp.path()).await;
    unsafe {
        std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
    }

    assert_eq!(
        result, "agnes",
        "empty env var must be treated like missing"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn no_env_no_config_defaults_to_dudu() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    // no config.toml at all

    unsafe {
        std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
    }

    let result = get_default_agent(tmp.path()).await;
    assert_eq!(result, "dudu", "final fallback must be 'dudu'");
}

// ── WP21 debt ⑧ — token binding ────────────────────────────────────────

fn set_claim(id: &str, token: &str) {
    // SAFETY: env mutation serialized via ENV_LOCK by every caller.
    unsafe {
        std::env::set_var(duduclaw_core::ENV_AGENT_ID, id);
        std::env::set_var(duduclaw_core::ENV_AGENT_TOKEN, token);
    }
}

fn clear_claim() {
    unsafe {
        std::env::remove_var(duduclaw_core::ENV_AGENT_ID);
        std::env::remove_var(duduclaw_core::ENV_AGENT_TOKEN);
    }
}

/// No `identity.key` ⇒ zero impact, even for an obviously forged token.
#[tokio::test(flavor = "current_thread")]
async fn no_identity_key_means_zero_behaviour_change() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_default_agent_config(tmp.path(), "agnes");

    set_claim("ceo", "obviously-forged");
    let result = get_default_agent(tmp.path()).await;
    clear_claim();

    assert_eq!(result, "ceo", "no key ⇒ pre-WP21 behaviour verbatim");
}

/// Soft mode (the default): an unsigned claim still works, so upgrading an
/// install that has a key but stale `.mcp.json` files does not break it.
#[tokio::test(flavor = "current_thread")]
async fn soft_mode_warns_but_accepts_unsigned_claim() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_default_agent_config(tmp.path(), "agnes");
    duduclaw_core::ensure_identity_key(tmp.path()).unwrap();

    set_claim("ceo", "");
    let result = get_default_agent(tmp.path()).await;
    clear_claim();

    assert_eq!(
        result, "ceo",
        "soft mode must not change the resolved caller"
    );
}

/// Strict mode: a valid token still resolves normally...
#[tokio::test(flavor = "current_thread")]
async fn strict_mode_accepts_a_valid_token() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    fs::write(
        tmp.path().join("config.toml"),
        "[general]\ndefault_agent = \"agnes\"\n\n\
             [delegation]\nrequire_identity_token = true\n",
    )
    .unwrap();
    let key = duduclaw_core::ensure_identity_key(tmp.path()).unwrap();

    set_claim(
        "sales-rep",
        &duduclaw_core::mint_identity_token(&key, "sales-rep"),
    );
    let result = get_default_agent(tmp.path()).await;
    clear_claim();

    assert_eq!(result, "sales-rep");
}

/// ...and a forged / replayed / absent one collapses to the untrusted
/// sentinel, which no WP21 gate will authorize for anything.
#[tokio::test(flavor = "current_thread")]
async fn strict_mode_rejects_forged_replayed_and_absent_claims() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    fs::write(
        tmp.path().join("config.toml"),
        "[general]\ndefault_agent = \"agnes\"\n\n\
             [delegation]\nrequire_identity_token = true\n",
    )
    .unwrap();
    let key = duduclaw_core::ensure_identity_key(tmp.path()).unwrap();
    let rep_token = duduclaw_core::mint_identity_token(&key, "sales-rep");

    for (id, token) in [
        ("ceo", ""),                 // the bare env-var impersonation
        ("ceo", "deadbeef"),         // garbage token
        ("ceo", rep_token.as_str()), // replay someone else's valid token
    ] {
        set_claim(id, token);
        let result = get_default_agent(tmp.path()).await;
        assert_eq!(
            result,
            duduclaw_core::UNTRUSTED_AGENT_ID,
            "strict mode must refuse {id}/{token}"
        );
        // And the server itself refuses to boot rather than serve it.
        assert!(
            super::run_mcp_server(tmp.path()).await.is_err(),
            "run_mcp_server must fail closed for {id}/{token}"
        );
        clear_claim();
    }

    // Dropping the env var entirely is also an escalation route (inherit
    // `default_agent`'s authority), so strict mode refuses that too.
    clear_claim();
    assert_eq!(
        get_default_agent(tmp.path()).await,
        duduclaw_core::UNTRUSTED_AGENT_ID
    );
}

/// The untrusted sentinel loses every org gate — the property that makes
/// the sentinel approach safe without touching each gate individually.
#[tokio::test(flavor = "current_thread")]
async fn untrusted_sentinel_is_denied_by_the_org_gates() {
    let tmp = TempDir::new();
    let agents = tmp.path().join("agents");
    fs::create_dir_all(agents.join("ceo")).unwrap();
    fs::write(
        agents.join("ceo").join("agent.toml"),
        "[agent]\nname = \"ceo\"\ndisplay_name = \"CEO\"\nrole = \"main\"\n\
             status = \"active\"\ntrigger = \"@ceo\"\nreports_to = \"\"\nicon = \"👑\"\n",
    )
    .unwrap();

    let sentinel = duduclaw_core::UNTRUSTED_AGENT_ID;
    assert!(!duduclaw_core::is_system_sender(sentinel));
    assert!(
        super::check_org_subject_allowed(tmp.path(), sentinel, "ceo", "t")
            .await
            .is_err()
    );
    assert!(
        super::check_org_placement_allowed(tmp.path(), sentinel, "ceo", "t")
            .await
            .is_err()
    );
}

// ── WP1.1 C2 — SOUL.md 唯讀化 identity gate for `agent_update_soul` ─────
//
// `DESIGN-evolution-v3-aee.md` §1.9.2 / §1.11 test names:
// `dashboard_principal_can_still_update_soul`,
// `agent_principal_is_denied_and_audited`.

fn write_agent_with_soul(home: &std::path::Path, agent_id: &str, can_modify_own_soul: bool) {
    let dir = home.join("agents").join(agent_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("agent.toml"),
        format!(
            "[agent]\nname = \"{agent_id}\"\ndisplay_name = \"T\"\nrole = \"worker\"\n\
                 status = \"active\"\ntrigger = \"manual\"\nreports_to = \"\"\nicon = \"\"\n\n\
                 [permissions]\ncan_modify_own_soul = {can_modify_own_soul}\n"
        ),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "# original soul\n").unwrap();
}

fn soul_update_params(agent_id: &str, content: &str) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "content": content })
}

/// Dashboard / operator convention: no `DUDUCLAW_AGENT_ID` at all in the
/// process env ⇒ unrestricted, matching `HookCaller::Absent`.
#[tokio::test(flavor = "current_thread")]
async fn dashboard_principal_can_still_update_soul() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_agent_with_soul(tmp.path(), "ceo", false);
    clear_claim();

    let result =
        super::handle_agent_update_soul(&soul_update_params("ceo", "# new soul\n"), tmp.path())
            .await;

    assert!(
        result.get("isError").is_none(),
        "operator caller must not be denied: {result}"
    );
    let written =
        fs::read_to_string(tmp.path().join("agents").join("ceo").join("SOUL.md")).unwrap();
    assert_eq!(written, "# new soul\n");
}

/// The B3 finding this WP closes: before it, the in-process agent MCP
/// principal (`Scope::Admin` by default) could call this tool on itself
/// with no gate at all. `can_modify_own_soul` defaults to `false` on
/// every shipped template, so the default behaviour must now deny.
#[tokio::test(flavor = "current_thread")]
async fn agent_principal_is_denied_and_audited() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_agent_with_soul(tmp.path(), "ceo", false);
    let before =
        fs::read_to_string(tmp.path().join("agents").join("ceo").join("SOUL.md")).unwrap();

    set_claim("ceo", "");
    let result =
        super::handle_agent_update_soul(&soul_update_params("ceo", "# hijacked\n"), tmp.path())
            .await;
    clear_claim();

    assert_eq!(
        result.get("isError").and_then(|v| v.as_bool()),
        Some(true),
        "agent principal must be denied: {result}"
    );
    let after =
        fs::read_to_string(tmp.path().join("agents").join("ceo").join("SOUL.md")).unwrap();
    assert_eq!(
        after, before,
        "SOUL.md must be unchanged after a denied call"
    );

    let audit = fs::read_to_string(tmp.path().join("tool_calls.jsonl")).unwrap_or_default();
    assert!(
        audit.contains("soul_write_denied"),
        "denial must be audited: {audit}"
    );
}

/// C4 escape hatch: `can_modify_own_soul = true` on the TARGET agent's
/// own config lets that agent write via MCP — but only to itself.
#[tokio::test(flavor = "current_thread")]
async fn can_modify_own_soul_flag_allows_self_write() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_agent_with_soul(tmp.path(), "lab-agent", true);

    set_claim("lab-agent", "");
    let result = super::handle_agent_update_soul(
        &soul_update_params("lab-agent", "# self-authored\n"),
        tmp.path(),
    )
    .await;
    clear_claim();

    assert!(
        result.get("isError").is_none(),
        "flagged agent must be allowed to self-write: {result}"
    );
    let written =
        fs::read_to_string(tmp.path().join("agents").join("lab-agent").join("SOUL.md"))
            .unwrap();
    assert_eq!(written, "# self-authored\n");
}

/// The flag is scoped to self-write only: an agent with its OWN flag set
/// to `true` still cannot rewrite a DIFFERENT agent's SOUL.md, even if
/// that other agent also opted in.
#[tokio::test(flavor = "current_thread")]
async fn can_modify_own_soul_does_not_grant_cross_agent_write() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_agent_with_soul(tmp.path(), "lab-agent", true);
    write_agent_with_soul(tmp.path(), "ceo", true);
    let before =
        fs::read_to_string(tmp.path().join("agents").join("ceo").join("SOUL.md")).unwrap();

    set_claim("lab-agent", "");
    let result = super::handle_agent_update_soul(
        &soul_update_params("ceo", "# cross-agent hijack\n"),
        tmp.path(),
    )
    .await;
    clear_claim();

    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
    let after =
        fs::read_to_string(tmp.path().join("agents").join("ceo").join("SOUL.md")).unwrap();
    assert_eq!(after, before, "cross-agent write must be denied");
}

/// `require_identity_token = true` + an unverifiable claim collapses to
/// the untrusted sentinel, which can never equal any real `agent_id` —
/// so even a target that opted into `can_modify_own_soul` stays denied.
#[tokio::test(flavor = "current_thread")]
async fn strict_mode_unverified_claim_is_denied_even_with_flag_set() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    fs::write(
        tmp.path().join("config.toml"),
        "[general]\ndefault_agent = \"lab-agent\"\n\n\
             [delegation]\nrequire_identity_token = true\n",
    )
    .unwrap();
    duduclaw_core::ensure_identity_key(tmp.path()).unwrap();
    write_agent_with_soul(tmp.path(), "lab-agent", true);

    // Claims an id but supplies no token — under strict mode this is
    // `IdentityVerdict::Rejected`, not `Unverified`.
    set_claim("lab-agent", "");
    let result = super::handle_agent_update_soul(
        &soul_update_params("lab-agent", "# forged\n"),
        tmp.path(),
    )
    .await;
    clear_claim();

    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
}

// ── System-sender names are never a process identity ─────────────────────

/// A self-asserted `DUDUCLAW_AGENT_ID=dashboard` (or `cron`, …) would inherit
/// the system senders' unconditional delegation reach; no gateway spawn path
/// ever stamps one, so it resolves to the untrusted sentinel.
#[tokio::test(flavor = "current_thread")]
async fn system_sender_name_as_identity_is_untrusted() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new();
    write_default_agent_config(tmp.path(), "agnes");

    for sender in duduclaw_core::SYSTEM_SENDERS {
        set_claim(sender, "");
        let result = get_default_agent(tmp.path()).await;
        clear_claim();
        assert_eq!(result, duduclaw_core::UNTRUSTED_AGENT_ID, "{sender}");
    }

    // The config fallback is held to the same rule.
    write_default_agent_config(tmp.path(), "cron");
    clear_claim();
    let result = get_default_agent(tmp.path()).await;
    assert_eq!(result, duduclaw_core::UNTRUSTED_AGENT_ID);

    // P2-A L4-2: an employee created under a queue sender name before it
    // was reserved (`heartbeat-scheduler`, `workflow`) is untrusted too.
    write_default_agent_config(tmp.path(), "agnes");
    for name in duduclaw_core::RESERVED_QUEUE_SENDERS {
        set_claim(name, "");
        let result = get_default_agent(tmp.path()).await;
        clear_claim();
        assert_eq!(result, duduclaw_core::UNTRUSTED_AGENT_ID, "{name}");
    }

    // An ordinary id is unaffected.
    set_claim("sales-rep", "");
    let result = get_default_agent(tmp.path()).await;
    clear_claim();
    assert_eq!(result, "sales-rep");
}

/// Rows of `tool_calls.jsonl` recording a refused system-sender identity.
fn identity_refusals(home: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(home.join("tool_calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r["reason"] == "system_sender_identity")
        .collect()
}

/// Replacing a claimed system-sender name with the sentinel must leave a
/// trace of WHAT was claimed and WHERE it came from — everything downstream
/// only ever sees `__untrusted__`. Once per process per home: the resolver
/// runs several times in one process.
#[tokio::test(flavor = "current_thread")]
async fn system_sender_identity_refusal_is_audited_once_with_claim_and_source() {
    let _guard = ENV_LOCK.lock().unwrap();

    let env_home = TempDir::new();
    write_default_agent_config(env_home.path(), "agnes");
    set_claim("heartbeat", "");
    let first = get_default_agent(env_home.path()).await;
    let second = get_default_agent(env_home.path()).await;
    clear_claim();
    assert_eq!(first, duduclaw_core::UNTRUSTED_AGENT_ID);
    assert_eq!(second, duduclaw_core::UNTRUSTED_AGENT_ID);
    let rows = identity_refusals(env_home.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["claimed"], "heartbeat");
    assert_eq!(rows[0]["source"], "env");

    let cfg_home = TempDir::new();
    write_default_agent_config(cfg_home.path(), "autopilot");
    clear_claim();
    let _ = get_default_agent(cfg_home.path()).await;
    let _ = get_default_agent(cfg_home.path()).await;
    let rows = identity_refusals(cfg_home.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["claimed"], "autopilot");
    assert_eq!(rows[0]["source"], "config");

    // An ordinary identity leaves no such row.
    let ok_home = TempDir::new();
    write_default_agent_config(ok_home.path(), "agnes");
    let _ = get_default_agent(ok_home.path()).await;
    assert!(identity_refusals(ok_home.path()).is_empty());
}
