//! RFC-23 §14.4 — data-file guard, the decision half.
//!
//! Keeps Claude Code's built-in file-reading route from bypassing DuDuClaw's
//! de-identification: `Read` and `Bash` are NOT MCP tools, so whatever they
//! return goes straight into the model's context without passing the redaction
//! choke point. `csv_read` / `xlsx_read` / `file_read` are the de-identified
//! route; this guard is what makes the model take it.
//!
//! ## Why this lives in `duduclaw-core`
//!
//! H10 (2026-09 feature audit) replaced the original `data-file-guard.sh` with
//! the Rust subcommand `duduclaw hook data-file-guard`, for the reason the
//! shell version documented against itself: **a shell script silently does
//! nothing on a Windows host with no bash on PATH.** Claude Code treats a
//! non-2 exit (including "command not found") as *allow*, so the guard was
//! absent exactly where nobody would notice. Its sibling
//! [`crate::agent_guard`] has been a Rust subcommand since v1.3.15 for the
//! same reason; the decision lives here so the CLI subcommand and the
//! gateway's installer tests share one implementation.
//!
//! ## Contract
//!
//! Same as `duduclaw hook agent-file-guard`: exit 0 → allow, exit 2 + stderr →
//! block, and stderr is shown to the model.
//!
//! ## Honest limitation — heuristic, not a sandbox
//!
//! The `Bash` check matches filenames. A command that builds its path
//! dynamically (`python -c "open(chr(99)+...)"`) walks past it. The real
//! protection is the MCP tool surface; this guard lowers the odds of the model
//! taking the wrong road by accident. That was true of the shell version too —
//! porting it to Rust closed the Windows hole, not this one.

/// Enforcement level, from `DUDUCLAW_DATA_FILE_GUARD` (set by the gateway at
/// spawn time, and only when redaction is actually active for that agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFileGuardMode {
    /// Block `Read` of a data file AND a `Bash` command naming one.
    On,
    /// Block `Read` only.
    ReadOnly,
    /// Allow everything — byte-identical to not having the hook at all.
    /// This is what an unset, empty, or unrecognised env value means.
    Off,
}

impl DataFileGuardMode {
    /// Parse the env value. Anything that is not exactly `on` / `read_only`
    /// is [`Off`](Self::Off): an operator typo must leave the agent working,
    /// not half-guarded in a way nobody can predict.
    pub fn from_env_value(raw: Option<&str>) -> Self {
        match raw.unwrap_or("").trim() {
            "on" => Self::On,
            "read_only" => Self::ReadOnly,
            _ => Self::Off,
        }
    }
}

/// Data-file extensions the guard recognises, lowercase. Matching is
/// case-insensitive.
pub const DATA_FILE_EXTENSIONS: &[&str] = &["csv", "tsv", "xlsx", "xlsm", "xls", "ods"];

/// The message shown to the model on a block (stderr line 1).
pub const DENY_MESSAGE: &str = "此檔案受去識別化保護，請改用 csv_read／xlsx_read／file_read";

/// What the hook should do with one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataFileDecision {
    /// Exit 0.
    Allow,
    /// Exit 2, with `reason` appended to [`DENY_MESSAGE`] on stderr.
    Block { reason: String },
}

impl DataFileDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Decide one PreToolUse call.
///
/// - `tool_name` — `Read` / `Bash` / anything else (anything else allows).
/// - `field_value` — `tool_input.file_path` for `Read`,
///   `tool_input.command` for `Bash`. An empty value allows: with nothing to
///   inspect the guard has no basis to block, and fail-open on a shape we do
///   not understand is the same posture `agent-file-guard` takes on a
///   malformed envelope.
///
/// `Write` / `Edit` are deliberately NOT covered — this guard is about
/// *reading* customer data, not about writing files.
pub fn decide(mode: DataFileGuardMode, tool_name: &str, field_value: &str) -> DataFileDecision {
    if mode == DataFileGuardMode::Off || field_value.is_empty() {
        return DataFileDecision::Allow;
    }
    match tool_name {
        "Read" => {
            if path_is_data_file(field_value) {
                DataFileDecision::Block {
                    reason: format!("Read {field_value}"),
                }
            } else {
                DataFileDecision::Allow
            }
        }
        "Bash" => {
            if mode != DataFileGuardMode::On {
                return DataFileDecision::Allow;
            }
            if command_names_data_file(field_value) {
                // The command itself is deliberately NOT echoed back: it can
                // carry customer data, and the model already knows what it
                // just tried to run.
                DataFileDecision::Block {
                    reason: "Bash".to_string(),
                }
            } else {
                DataFileDecision::Allow
            }
        }
        _ => DataFileDecision::Allow,
    }
}

/// Does this path end in a data-file extension (ignoring trailing whitespace)?
///
/// Deliberately not `std::path::Path::extension`: the value arrives verbatim
/// from the model and may be a Windows path, a quoted string, or carry
/// trailing spaces. Comparison is over `char`s, never byte indices, so a CJK
/// filename cannot panic (project convention 1).
pub fn path_is_data_file(path: &str) -> bool {
    let lower = path.trim_end().to_lowercase();
    DATA_FILE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Does this shell command name a data file?
///
/// Matches a token ending in a data-file extension where the character after
/// the extension does not continue the word — so `report.xlsx`, `"a.csv"` and
/// `x.csv;` all match while `.xlsxnote` does not. Same rule the shell version
/// implemented with `grep -Eiq`, restated without a regex engine.
pub fn command_names_data_file(command: &str) -> bool {
    let lower = command.to_lowercase();
    let bytes: Vec<char> = lower.chars().collect();
    for ext in DATA_FILE_EXTENSIONS {
        let needle: Vec<char> = std::iter::once('.').chain(ext.chars()).collect();
        let mut i = 0usize;
        while i + needle.len() <= bytes.len() {
            if bytes[i..i + needle.len()] == needle[..] {
                let after = bytes.get(i + needle.len());
                let continues_word = after
                    .map(|c| c.is_alphanumeric() || *c == '_')
                    .unwrap_or(false);
                if !continues_word {
                    return true;
                }
            }
            i += 1;
        }
    }
    false
}

/// Extract `(tool_name, field_value)` from a Claude Code PreToolUse envelope.
///
/// `None` ⇒ nothing to judge (fail open). This is the whole "JSON parsing"
/// half the shell version needed python3 (with a `sed` fallback that could
/// truncate on an escaped quote) to do.
pub fn extract_from_envelope(envelope: &serde_json::Value) -> Option<(String, String)> {
    let tool_name = envelope.get("tool_name")?.as_str()?.to_string();
    let field = match tool_name.as_str() {
        "Read" => envelope.pointer("/tool_input/file_path"),
        "Bash" => envelope.pointer("/tool_input/command"),
        _ => None,
    }?;
    Some((tool_name, field.as_str()?.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mode(s: &str) -> DataFileGuardMode {
        DataFileGuardMode::from_env_value(Some(s))
    }

    #[test]
    fn mode_parsing_is_strict_and_defaults_to_off() {
        assert_eq!(mode("on"), DataFileGuardMode::On);
        assert_eq!(mode("read_only"), DataFileGuardMode::ReadOnly);
        for raw in ["off", "", "nonsense", "ON", "Read_Only", " on x"] {
            assert_eq!(mode(raw), DataFileGuardMode::Off, "{raw:?} must read as off");
        }
        assert_eq!(
            DataFileGuardMode::from_env_value(None),
            DataFileGuardMode::Off
        );
        // Surrounding whitespace is tolerated — an env var set from a script
        // often carries a trailing newline.
        assert_eq!(mode(" on \n"), DataFileGuardMode::On);
    }

    #[test]
    fn read_of_a_data_file_is_blocked_and_other_files_pass() {
        assert!(!decide(mode("on"), "Read", "/w/customers.csv").is_allowed());
        assert!(decide(mode("on"), "Read", "/w/notes.md").is_allowed());
        assert!(decide(mode("on"), "Read", "/w/README").is_allowed());
    }

    #[test]
    fn every_spreadsheet_extension_including_cjk_names_and_case() {
        for ext in ["csv", "tsv", "xlsx", "xlsm", "xls", "ods", "XLSX", "CsV"] {
            let path = format!("/w/客戶清單.{ext}");
            assert!(
                !decide(mode("on"), "Read", &path).is_allowed(),
                "extension {ext} must be blocked"
            );
        }
        // A name that merely contains the letters must not match.
        assert!(decide(mode("on"), "Read", "/w/notes.xlsxnote").is_allowed());
        assert!(decide(mode("on"), "Read", "/w/csv").is_allowed());
    }

    #[test]
    fn trailing_whitespace_does_not_smuggle_a_data_file_past() {
        assert!(!decide(mode("on"), "Read", "/w/customers.csv  ").is_allowed());
    }

    #[test]
    fn bash_naming_a_data_file_is_blocked_in_on_mode_only() {
        assert!(!decide(mode("on"), "Bash", "head customers.csv").is_allowed());
        assert!(!decide(mode("on"), "Bash", "cat \"客戶.xlsx\" | less").is_allowed());
        assert!(!decide(mode("on"), "Bash", "wc -l a.csv; echo done").is_allowed());
        assert!(decide(mode("on"), "Bash", "echo hello world").is_allowed());
        assert!(decide(mode("on"), "Bash", "cat notes.xlsxnote").is_allowed());

        // read_only leaves Bash alone but still gates Read.
        assert!(decide(mode("read_only"), "Bash", "head customers.csv").is_allowed());
        assert!(!decide(mode("read_only"), "Read", "/w/a.xlsx").is_allowed());
    }

    #[test]
    fn off_mode_and_unrelated_tools_are_inert() {
        assert!(decide(mode("off"), "Read", "/w/customers.csv").is_allowed());
        assert!(decide(mode(""), "Bash", "head customers.csv").is_allowed());
        // Write is outside this guard's remit even in `on` mode.
        assert!(decide(mode("on"), "Write", "/w/out.csv").is_allowed());
        // Empty field value ⇒ nothing to judge.
        assert!(decide(mode("on"), "Read", "").is_allowed());
    }

    #[test]
    fn envelope_extraction_handles_each_shape_and_fails_open() {
        let (t, v) = extract_from_envelope(&json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/w/a.csv"}
        }))
        .unwrap();
        assert_eq!((t.as_str(), v.as_str()), ("Read", "/w/a.csv"));

        let (t, v) = extract_from_envelope(&json!({
            "tool_name": "Bash",
            "tool_input": {"command": "head a.csv"}
        }))
        .unwrap();
        assert_eq!((t.as_str(), v.as_str()), ("Bash", "head a.csv"));

        // A JSON-escaped quote inside the command is handled exactly — this is
        // the case the shell version's `sed` fallback truncated.
        let (_, v) = extract_from_envelope(&json!({
            "tool_name": "Bash",
            "tool_input": {"command": "grep \"a,b\" customers.csv"}
        }))
        .unwrap();
        assert!(v.contains("customers.csv"));
        assert!(command_names_data_file(&v));

        // Shapes with nothing to judge.
        assert!(extract_from_envelope(&json!({})).is_none());
        assert!(extract_from_envelope(&json!({"tool_name": "Write"})).is_none());
        assert!(
            extract_from_envelope(&json!({"tool_name": "Read", "tool_input": {}})).is_none(),
            "missing file_path ⇒ nothing to judge"
        );
        assert!(
            extract_from_envelope(&json!({"tool_name": "Read", "tool_input": {"file_path": 7}}))
                .is_none(),
            "non-string file_path ⇒ nothing to judge"
        );
    }
}
