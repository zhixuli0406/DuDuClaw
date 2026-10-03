//! Test-only shims with the handlers' pre-`RecordActor` signatures, so the
//! existing task-board / agent tests keep calling them with a bare caller id.
//!
//! A bare id stands for that AI employee (`RecordActor::Agent`), except a
//! system-sender name (`dashboard`, `cron`, …), which those tests use for the
//! human interface and which maps to `RecordActor::Operator`: no MCP caller
//! can present a system-sender identity in production (`get_default_agent`
//! turns one into the untrusted sentinel), so the only real caller those
//! tests can stand for is an operator.

use super::*;

pub(crate) fn actor(caller: &str) -> RecordActor<'_> {
    if duduclaw_core::is_system_sender(caller) {
        RecordActor::Operator(caller)
    } else {
        RecordActor::Agent(caller)
    }
}

pub(crate) async fn handle_tasks_create(args: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_tasks_create(args, home_dir, actor(caller)).await
}

pub(crate) async fn handle_tasks_update(args: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_tasks_update(args, home_dir, actor(caller)).await
}

pub(crate) async fn handle_tasks_claim(args: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_tasks_claim(args, home_dir, actor(caller)).await
}

pub(crate) async fn handle_tasks_complete(args: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_tasks_complete(args, home_dir, actor(caller)).await
}

pub(crate) async fn handle_tasks_block(args: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_tasks_block(args, home_dir, actor(caller)).await
}

pub(crate) async fn handle_agent_update(params: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_agent_update(params, home_dir, actor(caller)).await
}

pub(crate) async fn handle_activity_post(args: &Value, home_dir: &Path, caller: &str) -> Value {
    super::handle_activity_post(args, home_dir, actor(caller)).await
}
