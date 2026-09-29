use super::*;
use duduclaw_core::task_packet::{OutputFormat, TaskPacket};
use duduclaw_core::types::Role;
use std::fs;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("duduclaw-th-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
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

/// Scaffold a role member the way WP-2's ephemeral spawn does — including
/// the `parent` key, which `read_team_member_identity` must tolerate
/// (`ephemeral::scaffold_with` writes all four of role/task_id/round/parent).
fn mk_member(
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
}

fn mk_member_in(dir: &std::path::Path, role: &str, task: Option<&str>, round: Option<u32>) {
    fs::create_dir_all(dir).unwrap();
    let mut toml = format!("[team_member]\nparent = \"agnes\"\nrole = \"{role}\"\n");
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
