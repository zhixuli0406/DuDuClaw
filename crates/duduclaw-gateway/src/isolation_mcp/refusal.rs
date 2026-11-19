//! R8t: why an isolated run failed, for operators — without trusting stderr.
//!
//! The runtime's stderr comes from a process running as the employee inside
//! the sandbox, so it is untrusted. It is never shown, logged or interpreted:
//! [`refusal_reason`] only *selects* one entry of the fixed allow-list shared
//! with the API worker (`duduclaw_api_worker::REFUSAL_REASONS`) and returns
//! that entry's own `&'static str`. Everything derived from it (class, log
//! fields, end-user message) is a fixed string from this module.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use duduclaw_api_worker::{REFUSAL_LINE_PREFIX, REFUSAL_REASONS};

/// Only the last 4 KiB of the drained stderr are looked at.
pub(crate) const SCAN_LIMIT: usize = 4 * 1024;

/// The protected-mode default refusal (also every unrecognised failure).
pub(crate) const DEFAULT_MESSAGE: &str =
    "受保護模式尚無這次工作的隔離執行授權，或所選執行設定尚未通過驗證；請由管理員檢查服務狀態。";

/// The last complete line within the last [`SCAN_LIMIT`] bytes that is
/// exactly `REFUSAL_LINE_PREFIX + reason` for an allow-listed reason. A line
/// cut by the window, or carrying anything else (`\r`, spaces, escapes, NUL),
/// is not recognised. The result is the allow-list's own string, never a
/// slice of `stderr`.
pub(crate) fn refusal_reason(stderr: &[u8]) -> Option<&'static str> {
    let start = stderr.len().saturating_sub(SCAN_LIMIT);
    let mut tail = &stderr[start..];
    if start > 0 && stderr[start - 1] != b'\n' {
        // The window starts inside a line: that partial line is not a line.
        let newline = tail.iter().position(|byte| *byte == b'\n')?;
        tail = &tail[newline + 1..];
    }
    tail.split(|byte| *byte == b'\n').rev().find_map(|line| {
        let reason = line.strip_prefix(REFUSAL_LINE_PREFIX.as_bytes())?;
        REFUSAL_REASONS.iter().copied().find(|known| known.as_bytes() == reason)
    })
}

/// Fixed operator-facing classes of the worker's refusal reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalClass {
    Launch, Input, McpProjection, EmployeeCredentials, ProviderTransport, ProviderRefused,
    ProviderResponse, McpBridge, Timeout, Limits, Output,
}

impl RefusalClass {
    pub(crate) const ALL: [Self; 11] = [Self::Launch, Self::Input, Self::McpProjection, Self::EmployeeCredentials,
        Self::ProviderTransport, Self::ProviderRefused, Self::ProviderResponse, Self::McpBridge, Self::Timeout,
        Self::Limits, Self::Output];
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Input => "input",
            Self::McpProjection => "mcp_projection",
            Self::EmployeeCredentials => "employee_credentials",
            Self::ProviderTransport => "provider_transport",
            Self::ProviderRefused => "provider_refused",
            Self::ProviderResponse => "provider_response",
            Self::McpBridge => "mcp_bridge",
            Self::Timeout => "timeout",
            Self::Limits => "limits",
            Self::Output => "output",
        }
    }
}

use self::RefusalClass as C;

/// Every allow-listed reason with exactly one class (enforced by tests).
const CLASSES: &[(&str, RefusalClass)] = &[
    ("worker launch refused", C::Launch), ("unsupported platform", C::Launch),
    ("sealed input unavailable", C::Input), ("sealed input invalid", C::Input), ("sealed input read failed", C::Input),
    ("input fencing failed", C::Input), ("input timeout", C::Input), ("input read failed", C::Input),
    ("input mismatch", C::Input), ("request invalid", C::Input), ("invalid request", C::Input),
    ("request too large", C::Input), ("provider request invalid", C::Input), ("invalid Gemini model", C::Input),
    ("MCP projection unavailable", C::McpProjection), ("MCP projection invalid", C::McpProjection),
    ("invalid MCP projection", C::McpProjection),
    ("employee credentials unavailable", C::EmployeeCredentials),
    ("employee credentials invalid", C::EmployeeCredentials),
    ("employee credentials too large", C::EmployeeCredentials),
    ("invalid employee credentials", C::EmployeeCredentials), ("invalid employee credential", C::EmployeeCredentials),
    ("invalid endpoint", C::EmployeeCredentials), ("duplicate employee provider", C::EmployeeCredentials),
    ("employee provider required", C::EmployeeCredentials), ("employee provider unavailable", C::EmployeeCredentials),
    ("HTTP transport unavailable", C::ProviderTransport), ("provider transport failed", C::ProviderTransport),
    ("provider response failed", C::ProviderTransport),
    ("provider refused request", C::ProviderRefused),
    ("provider response invalid", C::ProviderResponse), ("provider usage missing", C::ProviderResponse),
    ("provider choice invalid", C::ProviderResponse), ("provider message missing", C::ProviderResponse),
    ("provider role invalid", C::ProviderResponse), ("provider calls invalid", C::ProviderResponse),
    ("provider completion incomplete", C::ProviderResponse), ("empty provider answer", C::ProviderResponse),
    ("invalid tool call", C::ProviderResponse), ("unadvertised tool", C::ProviderResponse),
    ("invalid tool arguments", C::ProviderResponse), ("duplicate tool call", C::ProviderResponse),
    ("invalid provider field", C::ProviderResponse), ("invalid tool replay", C::ProviderResponse),
    ("invalid replay", C::ProviderResponse), ("invalid replay protocol", C::ProviderResponse),
    ("missing tool replay", C::ProviderResponse), ("invalid role", C::ProviderResponse),
    ("invalid tool", C::ProviderResponse), ("invalid schema", C::ProviderResponse),
    ("invalid protocol", C::ProviderResponse), ("invalid provider message", C::ProviderResponse),
    ("invalid response", C::ProviderResponse), ("unsupported provider block", C::ProviderResponse),
    ("incomplete provider response", C::ProviderResponse), ("unsupported provider content", C::ProviderResponse),
    ("incomplete provider call", C::ProviderResponse), ("invalid candidates", C::ProviderResponse),
    ("invalid arguments", C::ProviderResponse), ("usage overflow", C::ProviderResponse),
    ("MCP bridge unavailable", C::McpBridge), ("MCP input unavailable", C::McpBridge),
    ("MCP output unavailable", C::McpBridge), ("MCP request invalid", C::McpBridge), ("MCP write failed", C::McpBridge),
    ("MCP read failed", C::McpBridge), ("MCP disconnected", C::McpBridge), ("MCP response invalid", C::McpBridge),
    ("MCP request refused", C::McpBridge), ("MCP result missing", C::McpBridge), ("MCP authority changed", C::McpBridge),
    ("incomplete tool inventory", C::McpBridge), ("invalid tool inventory", C::McpBridge),
    ("invalid tool name", C::McpBridge), ("invalid tool schema", C::McpBridge), ("tool result invalid", C::McpBridge),
    ("API execution timeout", C::Timeout),
    ("MCP request too large", C::Limits), ("MCP response too large", C::Limits), ("MCP notification limit", C::Limits),
    ("response too large", C::Limits), ("conversation too large", C::Limits),
    ("provider response too large", C::Limits), ("tool round limit reached", C::Limits),
    ("tool inventory too large", C::Limits),
    ("response invalid", C::Output), ("output failed", C::Output),
];

pub(crate) fn class_of(reason: &str) -> Option<RefusalClass> {
    CLASSES.iter().find(|(known, _)| *known == reason).map(|(_, class)| *class)
}

/// The end-user sentence for a refused protected API run: one of a few fixed
/// zh-TW sentences by class, otherwise [`DEFAULT_MESSAGE`]. No input content.
pub(crate) fn operator_message(class: Option<RefusalClass>) -> &'static str {
    match class {
        Some(RefusalClass::EmployeeCredentials) =>
            "這位 AI 員工尚未設定模型連線資料，或設定內容無法使用；請管理員到『受保護模式 → 模型連線資料』檢查。",
        Some(RefusalClass::ProviderTransport) => "連不到這位 AI 員工設定的模型服務；請管理員檢查端點位址與對外連線。",
        Some(RefusalClass::ProviderRefused) => "模型服務拒絕了這次請求；請管理員檢查這位 AI 員工的金鑰與模型名稱。",
        Some(RefusalClass::ProviderResponse) => "模型服務的回覆無法使用；請管理員檢查端點是否為相容的服務。",
        Some(RefusalClass::Timeout) => "這次工作超過時間上限而停止。",
        _ => DEFAULT_MESSAGE,
    }
}

/// The spawn-gate error carrying [`operator_message`]; the prefix keeps the
/// existing "stop rotation/fallbacks" handling of protected refusals.
pub(crate) fn user_error(class: Option<RefusalClass>) -> String {
    duduclaw_agent::mcp_spawn_gate::spawn_gate_error(operator_message(class))
}

/// How the run ended, for the `exit` log field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunExit { Code(i32), Signal, Timeout, Incomplete }

impl RunExit {
    pub(crate) fn from_status(status: std::process::ExitStatus) -> Self {
        status.code().map_or(Self::Signal, Self::Code)
    }
}

impl std::fmt::Display for RunExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "{code}"),
            Self::Signal => f.write_str("signal"),
            Self::Timeout => f.write_str("timeout"),
            Self::Incomplete => f.write_str("incomplete"),
        }
    }
}

/// The fixed `runtime` log field (a registered CLI's id is not logged).
pub(crate) fn runtime_label(runtime: &duduclaw_isolation::execution::RuntimeKind) -> &'static str {
    use duduclaw_isolation::execution::RuntimeKind;
    match runtime {
        RuntimeKind::Claude => "claude",
        RuntimeKind::Codex => "codex",
        RuntimeKind::Grok => "grok",
        RuntimeKind::OpenAiCompat => "openai_compat",
        RuntimeKind::Registered(_) => "registered",
    }
}

/// The single `isolated run refused` warning. Only fixed strings, the agent
/// label the neighbouring warnings use, the exit and the stderr *length*.
pub(crate) fn log_refused(runtime: &'static str, agent: &str, reason: Option<&'static str>, exit: RunExit,
    stderr_len: usize) {
    let class = reason.and_then(class_of).map_or("unrecognised", RefusalClass::as_str);
    tracing::warn!(runtime, agent, reason = reason.unwrap_or("unrecognised"), class, exit = %exit, stderr_len,
        "isolated run refused");
}

#[cfg(test)]
mod tests {
    use super::*;
    const PREFIX: &[u8] = REFUSAL_LINE_PREFIX.as_bytes();
    const MARKER: &str = "R8T-PLANTED-MARKER-7f3a";

    fn cat(parts: &[&[u8]]) -> Vec<u8> { parts.concat() }
    fn line(reason: &str) -> Vec<u8> { cat(&[PREFIX, reason.as_bytes(), b"\n"]) }

    #[test]
    fn recognised_line_returns_the_allow_list_entry_not_the_input() {
        for reason in REFUSAL_REASONS {
            let stderr = line(reason);
            let found = refusal_reason(&stderr).unwrap_or_else(|| panic!("{reason}"));
            assert_eq!(found, *reason);
            assert!(std::ptr::eq(found.as_ptr(), reason.as_ptr()), "{reason}");
            let input = stderr.as_ptr_range();
            assert!(!input.contains(&found.as_ptr()), "{reason}");
        }
        // Without the trailing newline too.
        let stderr = cat(&[PREFIX, b"input mismatch"]);
        assert_eq!(refusal_reason(&stderr), Some("input mismatch"));
    }

    #[test]
    fn unknown_or_decorated_reasons_are_not_recognised() {
        let known = line("input mismatch");
        for stderr in [
            line("not a known reason"), line(MARKER), line(""), line("input mismatch "), line(" input mismatch"),
            line("Input mismatch"), line("input mismatch\r"), line("input mismatch\0"),
            line("\u{1b}[31minput mismatch\u{1b}[0m"), line("input mismatch: extra"),
            cat(&[b" ", &known]), cat(&[b"\0", &known]), cat(&[b"\x1b[0m", &known]), cat(&[b"prefix ", &known]),
            b"isolated API execution unavailable:input mismatch\n".to_vec(),
            b"isolated API execution unavailable: input mismatch\r\n".to_vec(),
            Vec::new(), b"\n\n\n".to_vec(),
        ] {
            assert_eq!(refusal_reason(&stderr), None, "{:?}", String::from_utf8_lossy(&stderr));
        }
    }

    #[test]
    fn the_last_recognised_line_wins_and_noise_lines_are_skipped() {
        let stderr = cat(&[&line("input mismatch"), b"noise\n", &line("provider refused request"),
            &line("not a known reason"), b"trailing noise without newline"]);
        assert_eq!(refusal_reason(&stderr), Some("provider refused request"));
    }

    #[test]
    fn only_the_last_four_kib_are_scanned() {
        let noise = vec![b'x'; 8 * 1024];
        let target = line("API execution timeout");
        // Long noise ending in a newline, then the line: found.
        let stderr = cat(&[&noise, b"\n", &target]);
        assert_eq!(refusal_reason(&stderr), Some("API execution timeout"));
        // The line, then more than 4 KiB of noise: not found.
        assert_eq!(refusal_reason(&cat(&[&target, &noise])), None);
        // A line that starts one byte before the window is cut: not a line.
        let filler = vec![b'y'; SCAN_LIMIT - target.len() + 1];
        assert_eq!(refusal_reason(&cat(&[&noise, b"\n", &target, &filler])), None);
        // A window cut mid-line exactly at the prefix is not taken as a line.
        let filler = vec![b'\n'; SCAN_LIMIT - target.len()];
        let stderr = cat(&[b"AAAA", &target, &filler]);
        assert_eq!(stderr.len() - SCAN_LIMIT, 4);
        assert_eq!(refusal_reason(&stderr), None);
        // The same line starting exactly at the window, after a newline: found.
        let stderr = cat(&[&noise, b"\n", &target, &filler]);
        assert_eq!(refusal_reason(&stderr), Some("API execution timeout"));
    }

    #[test]
    fn invalid_utf8_around_the_line_is_tolerated() {
        let stderr = cat(&[b"\xff\xfe\xc3\n", &line("provider transport failed"), b"\x80\x80\n"]);
        assert_eq!(refusal_reason(&stderr), Some("provider transport failed"));
        assert_eq!(refusal_reason(&cat(&[PREFIX, b"provider transport failed\xff\n"])), None);
    }

    #[test]
    fn every_allow_listed_reason_has_exactly_one_class_and_no_extra_entry() {
        for reason in REFUSAL_REASONS {
            assert_eq!(CLASSES.iter().filter(|(known, _)| known == reason).count(), 1, "{reason}");
            assert!(class_of(reason).is_some(), "{reason}");
        }
        for (known, _) in CLASSES { assert!(REFUSAL_REASONS.contains(known), "{known}"); }
        assert_eq!(CLASSES.len(), REFUSAL_REASONS.len());
        for class in RefusalClass::ALL {
            assert!(CLASSES.iter().any(|(_, c)| *c == class), "{class:?} is unused");
        }
        assert_eq!(class_of("employee credentials unavailable"), Some(RefusalClass::EmployeeCredentials));
        assert_eq!(class_of("provider transport failed"), Some(RefusalClass::ProviderTransport));
        assert_eq!(class_of("provider refused request"), Some(RefusalClass::ProviderRefused));
        assert_eq!(class_of("provider response invalid"), Some(RefusalClass::ProviderResponse));
        assert_eq!(class_of("API execution timeout"), Some(RefusalClass::Timeout));
        assert_eq!(class_of(MARKER), None);
    }

    #[test]
    fn messages_are_fixed_spawn_gate_errors_without_input_bytes() {
        let mut distinct = std::collections::BTreeSet::new();
        for class in RefusalClass::ALL.into_iter().map(Some).chain([None]) {
            let error = user_error(class);
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
            assert_eq!(duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error), Some(operator_message(class)));
            distinct.insert(operator_message(class));
        }
        // Five per-class sentences plus the default.
        assert_eq!(distinct.len(), 6);
        assert_eq!(operator_message(None), DEFAULT_MESSAGE);
        assert_eq!(operator_message(Some(RefusalClass::McpBridge)), DEFAULT_MESSAGE);
        // A marker planted in stderr never reaches the message.
        for reason in REFUSAL_REASONS {
            let stderr = cat(&[MARKER.as_bytes(), b"\n", &line(reason), MARKER.as_bytes()]);
            let error = user_error(refusal_reason(&stderr).and_then(class_of));
            assert!(!error.contains(MARKER) && !error.contains(reason), "{reason}: {error}");
        }
    }

    #[derive(Clone)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    #[test]
    fn the_log_line_has_fixed_fields_and_never_stderr_bytes() {
        for (stderr, reason, class) in [
            (cat(&[MARKER.as_bytes(), b"\n", &line("provider refused request")]), "provider refused request", "provider_refused"),
            (cat(&[&line("provider refused request"), MARKER.as_bytes()]), "provider refused request", "provider_refused"),
            (line(MARKER), "unrecognised", "unrecognised"),
            (format!("{MARKER} sk-secret-key\n").into_bytes(), "unrecognised", "unrecognised"),
        ] {
            let log = LogCapture(Default::default());
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            tracing::subscriber::with_default(subscriber, ||
                log_refused("openai_compat", "alice", refusal_reason(&stderr), RunExit::Code(125), stderr.len()));
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert_eq!(logs.lines().count(), 1, "{logs}");
            assert!(logs.contains("isolated run refused"), "{logs}");
            assert!(!logs.contains(MARKER) && !logs.contains("sk-secret"), "{logs}");
            for field in ["runtime=\"openai_compat\"", "agent=\"alice\"", "exit=125"] {
                assert!(logs.contains(field), "{field}: {logs}");
            }
            assert!(logs.contains(&format!("reason=\"{reason}\"")), "{logs}");
            assert!(logs.contains(&format!("class=\"{class}\"")), "{logs}");
            assert!(logs.contains(&format!("stderr_len={}", stderr.len())), "{logs}");
        }
        assert_eq!(RunExit::Timeout.to_string(), "timeout");
        assert_eq!(RunExit::Signal.to_string(), "signal");
        assert_eq!(RunExit::Incomplete.to_string(), "incomplete");
    }

    #[test]
    fn runtime_labels_are_fixed() {
        use duduclaw_isolation::{bounded::Text, execution::RuntimeKind};
        let registered = RuntimeKind::Registered(Text::new("generic-cli-v1.qwen".into()).unwrap());
        assert_eq!(runtime_label(&registered), "registered");
        assert_eq!(runtime_label(&RuntimeKind::OpenAiCompat), "openai_compat");
    }
}
