//! Temporary fail-closed entry fence until each runtime uses the protected
//! main-run context. Never treat missing integration as an account failure.

#[derive(Clone, Copy)]
pub(super) enum PendingRuntime { Codex, Grok, GenericCli, OpenAiCompat }

pub(super) fn require_integrated(runtime: PendingRuntime) -> Result<(), String> {
    check(duduclaw_core::execution_gate::isolation_required(), runtime)
}

fn check(required: bool, runtime: PendingRuntime) -> Result<(), String> {
    if !required { return Ok(()); }
    let name = match runtime {
        PendingRuntime::Codex => "Codex",
        PendingRuntime::Grok => "Grok",
        PendingRuntime::GenericCli => "通用 CLI",
        PendingRuntime::OpenAiCompat => "OpenAI 相容執行環境",
    };
    Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
        "受保護模式尚未支援 {name} 的隔離執行，這次工作已停止；請由管理員確認執行環境的支援狀態。"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_runtimes_refuse_with_terminal_operator_message() {
        for runtime in [PendingRuntime::Codex, PendingRuntime::Grok,
            PendingRuntime::GenericCli, PendingRuntime::OpenAiCompat] {
            let error = check(true, runtime).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
            let message = duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error).unwrap();
            assert!(message.contains("隔離執行"));
            assert!(message.contains("管理員"));
        }
    }

    #[test]
    fn local_runtimes_remain_available_without_changing_the_process_latch() {
        for runtime in [PendingRuntime::Codex, PendingRuntime::Grok,
            PendingRuntime::GenericCli, PendingRuntime::OpenAiCompat] {
            assert!(check(false, runtime).is_ok());
        }
    }
}
