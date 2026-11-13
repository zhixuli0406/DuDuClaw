use super::*;
use duduclaw_core::task_packet::{OutputFormat, TaskPacket};
use duduclaw_core::types::Role;
use std::fs;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().canonicalize().unwrap().join(format!("duduclaw-th-test-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).unwrap();
        Self(path)
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

const TASK: &str = "6f1c2a8e-1111-4222-8333-444455556666";

/// Scaffold a VERIFIED role member the way WP-2's ephemeral spawn does —
/// under `agents/.ephemeral/<eph-id>/`, with `ephemeral.toml` naming the same
/// parent as `[team_member]` (RM-137 H1: nothing else is a member). A plain
/// `id` is turned into `eph-<id>`; the member id actually created is returned
/// and is what the caller must pass to the tool.
fn mk_member(
    home: &std::path::Path,
    id: &str,
    role: &str,
    task: Option<&str>,
    round: Option<u32>,
) -> String {
    let member = if duduclaw_gateway::ephemeral::is_ephemeral_id(id) {
        id.to_string()
    } else {
        format!("eph-{id}")
    };
    mk_ephemeral_member(home, &member, role, task, round);
    member
}

/// What RM-137 H1 closes: an ORDINARY employee (`agents/<id>/`) that writes a
/// complete, pinned `[team_member]` into its own `agent.toml`.
fn mk_forged_member(
    home: &std::path::Path,
    id: &str,
    role: &str,
    task: Option<&str>,
    round: Option<u32>,
) {
    mk_member_in(&home.join("agents").join(id), role, task, round);
}

/// The real production layout: a role member lives one level deeper, under
/// `agents/.ephemeral/<eph-id>/`.
fn mk_ephemeral_member(
    home: &std::path::Path,
    id: &str,
    role: &str,
    task: Option<&str>,
    round: Option<u32>,
) {
    mk_member_in(
        &home
            .join("agents")
            .join(duduclaw_gateway::ephemeral::EPHEMERAL_DIR_NAME)
            .join(id),
        role,
        task,
        round,
    );
    let dir = home.join("agents/.ephemeral").join(id);
    let parent = home.join("agents/agnes");
    fs::create_dir_all(&parent).unwrap();
    fs::write(parent.join("agent.toml"), "[agent]\nstatus = \"active\"\n").unwrap();
    fs::write(
        dir.join("ephemeral.toml"),
        "parent = \"agnes\"\ntier = \"standard\"\ncreated_at = \"2026-10-29T00:00:00Z\"\nexpires_at = \"2026-10-30T00:00:00Z\"\n",
    ).unwrap();
    duduclaw_core::org_store::with_org_lock(home, |org| {
        duduclaw_core::org_store::upsert(org, id, duduclaw_core::OrgEntry::new("agnes", ""))
    })
    .unwrap();
}

fn mk_member_in(dir: &std::path::Path, role: &str, task: Option<&str>, round: Option<u32>) {
    fs::create_dir_all(dir).unwrap();
    let mut toml = format!("[agent]\nstatus = \"active\"\nreports_to = \"agnes\"\n[team_member]\nparent = \"agnes\"\nrole = \"{role}\"\n");
    if let Some(t) = task {
        toml.push_str(&format!("task_id = \"{t}\"\n"));
    }
    if let Some(r) = round {
        toml.push_str(&format!("round = {r}\n"));
    }
    fs::write(dir.join("agent.toml"), toml).unwrap();
}

fn packet(from: Role, to: Role, round: u32) -> TaskPacket {
    let mut p = TaskPacket::new(
        "pk-1",
        TASK,
        round,
        from,
        to,
        "整理 Q2 到期合約清單",
        OutputFormat::Files,
    );
    p.constraints
        .push(duduclaw_core::task_packet::Constraint::new("c1", "只讀 Q2"));
    p.audience.push("verifier".to_string());
    p
}

fn is_error(v: &Value) -> bool {
    v.get("isError").and_then(|x| x.as_bool()).unwrap_or(false)
}

fn text_of(v: &Value) -> String {
    v["content"][0]["text"].as_str().unwrap().to_string()
}

fn refusal_code(v: &Value) -> String {
    assert!(is_error(v), "expected a refusal, got {v}");
    text_of(v).split(':').next().unwrap().to_string()
}

/// The handler as a gateway-spawned role member calls it: with the
/// token-verified identity of `agent` (RM-137 soft identity mode — an
/// unverified caller is refused, see `unverified_role_member_is_refused`).
/// Shadows the module's `handle_team_handoff` for every test in this module.
async fn handle_team_handoff(args: &Value, home: &std::path::Path, agent: &str) -> Value {
    crate::mcp::with_token_verified_caller(agent, super::handle_team_handoff(args, home, agent)).await
}

async fn call(home: &std::path::Path, agent: &str, packet: &TaskPacket) -> Value {
    handle_team_handoff(
        &serde_json::json!({ "packet": serde_json::to_value(packet).unwrap() }),
        home,
        agent,
    )
    .await
}

mod part1;
mod part2;
