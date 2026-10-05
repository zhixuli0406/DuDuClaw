//! Agent-structure write guard (CLI-S5 / Option 3 hardening).
//!
//! Prevents agents from silently creating parallel agent hierarchies outside
//! the canonical `<duduclaw_home>/agents/` directory by using the raw Write /
//! Edit tools. Agents should use the `create_agent` MCP tool instead.
//!
//! This is enforced via a Claude Code `PreToolUse` hook that runs the
//! `duduclaw hook agent-file-guard` subcommand. The subcommand delegates to
//! [`check_agent_file_write`] below.

use std::path::{Component, Path, PathBuf};

/// Filenames that indicate an agent-structure file.
///
/// Writes to these filenames are only allowed under `<home>/agents/<name>/`
/// (any depth below an agent directory is fine — e.g. `wiki/`, `SKILLS/`).
///
/// This intentionally covers the file that's *checked in to every agent*.
/// Additional sentinel files can be added here without touching call sites.
pub const AGENT_STRUCTURE_FILES: &[&str] = &[
    "agent.toml",
    "SOUL.md",
    "CLAUDE.md",
    "MEMORY.md",
    ".mcp.json",
    "CONTRACT.toml",
];

/// Outcome of the guard check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    /// Path is safe — let the tool call proceed.
    Allow,
    /// Path is a non-agent-structure file — not our concern.
    NotAgentFile,
    /// Path is an agent-structure file under the canonical agents dir.
    AllowedAgentWrite,
    /// Path is an agent-structure file but lives *outside* `<home>/agents/`.
    /// The caller should block the tool call and tell the user to use
    /// the `create_agent` MCP tool instead.
    BlockedOutsideHome {
        file_name: String,
        attempted_path: PathBuf,
    },
    /// WP21 欠帳 ②: the write would change `[agent] reports_to` / `department`
    /// / `name` in a canonical `agent.toml` — the very fields the A2A
    /// delegation predicate and the whitelist resolver read. Only
    /// `agent_update` / the dashboard (which carry the C4 authorization gate)
    /// may change them.
    BlockedOrgFieldChange {
        file_name: String,
        attempted_path: PathBuf,
        /// Human-readable `field：「before」→「after」` entries.
        changed: Vec<String>,
    },
    /// Team-as-Agent review P1: the write would change `[capabilities]` in a
    /// canonical `agent.toml` — the permission envelope every tool gate reads
    /// (`allowed_tools` / `denied_tools`, the `computer_use` /
    /// `browser_via_bash` / `os_native` / `git_credentials` master switches,
    /// `db_sources`, the approval / irreversible / scoped tool lists,
    /// `autonomy_level`, `wiki_visible_to`).
    ///
    /// Distinct from [`Self::BlockedOrgFieldChange`] only in the message: the
    /// org fields answer "who may command whom", these answer "what may this
    /// employee do at all". A role member of a team runs with the employee's
    /// workspace as its cwd, so the file-guard hook reads it as the employee
    /// itself — which made this file the one place a cheap third-party model
    /// could widen the very envelope that is supposed to contain it.
    BlockedProtectedField {
        file_name: String,
        attempted_path: PathBuf,
        /// Human-readable `capabilities.<key>：「before」→「after」` entries.
        changed: Vec<String>,
    },
    /// WP21 欠帳 ②: the write would change a protected section of
    /// `<home>/config.toml` (`[delegation]` / `[acp]`) — the policy and trust
    /// switches the delegation gate itself consults.
    BlockedProtectedSection {
        file_name: String,
        attempted_path: PathBuf,
        changed: Vec<String>,
    },
    /// WP21 欠帳 ②: the write targets a delegation-authority file but its
    /// effect could not be verified (unparseable TOML on either side, or an
    /// edit whose resulting content could not be reconstructed). Fail closed.
    BlockedUnverifiable {
        file_name: String,
        attempted_path: PathBuf,
        reason: String,
    },
    /// WP21 欠帳 ②: a Bash command that looks like it writes a
    /// delegation-authority file. Heuristic — see
    /// [`crate::org_field_guard::check_bash_protected_write`].
    BlockedBashProtectedWrite {
        file_name: String,
        /// The write-shaped fragment that matched (for the message).
        verb: String,
    },
    /// WP21 review follow-up: the write targets a file that decides *who the
    /// caller is* (`identity.key`, an agent's `.mcp.json` identity env) or
    /// *whether this guard runs at all* (`.claude/settings.json`). See
    /// [`crate::org_field_guard::check_identity_surface_write`].
    BlockedIdentitySurface {
        file_name: String,
        attempted_path: PathBuf,
        reason: String,
    },
    /// WP22 T2: the write targets **another** agent's directory. The WP21
    /// content guards had no notion of *who* was writing, so agent A could
    /// still rewrite agent B's `SOUL.md` / `MEMORY.md` / anything else. See
    /// [`crate::org_field_guard::check_caller_scope`].
    BlockedForeignAgentDir {
        /// The caller's own agent id (from `DUDUCLAW_AGENT_ID`).
        caller: String,
        /// The agent directory that owns the target path.
        owner: String,
        attempted_path: PathBuf,
    },
    /// WP22 T2: `require_identity_token = true` and the caller's identity claim
    /// failed verification — every write under `<home>/agents/` is refused,
    /// because there is no trustworthy way to tell whose directory this is.
    BlockedUntrustedCaller {
        /// The (unverified) claimed id, for the operator-facing message.
        caller: String,
        attempted_path: PathBuf,
    },
    /// WP22 T2: an agent-identified caller tried to write `<home>/config.toml`.
    /// WP21 only froze `[delegation]` / `[acp]`; the whole file is now off
    /// limits to agents (every legitimate writer goes through Rust, not this
    /// hook). Operators without an agent identity are unaffected.
    BlockedHomeConfigWrite {
        caller: String,
        attempted_path: PathBuf,
    },
    /// WP1.1 C3 (SOUL.md 唯讀化, `DESIGN-evolution-v3-aee.md` §1.9.2): an
    /// agent-identified caller tried to write `SOUL.md` inside its OWN agent
    /// directory via Write/Edit/Bash. [`Self::BlockedForeignAgentDir`] only
    /// covers *another* agent's directory — a caller writing inside its own
    /// directory previously fell through as [`Self::NotAgentFile`]. SOUL.md
    /// is now off-limits even to its owner: personality is operator-managed
    /// (dashboard), mirroring the MCP-side `agent_update_soul` C2 gate. Not
    /// gated by `can_modify_own_soul` — that flag is the C2 gate's only
    /// escape hatch (design §1.9.2 C4); the one self-modification route is
    /// the `agent_update_soul` MCP tool.
    BlockedOwnSoulWrite {
        caller: String,
        attempted_path: PathBuf,
    },
    /// Contract lock: an agent-identified caller tried to write `CONTRACT.toml`
    /// inside its OWN agent directory via Write/Edit/MultiEdit/Bash. The
    /// contract's `must_not` / `must_always` boundaries are the operator's
    /// limits on the agent, so unlike SOUL.md there is no opt-in flag at all;
    /// the only write path is the dashboard's admin-only `contract.update`
    /// RPC, which never runs through this hook. Another agent's contract is
    /// refused earlier by [`Self::BlockedForeignAgentDir`].
    BlockedOwnContractWrite {
        caller: String,
        attempted_path: PathBuf,
    },
    /// Removed-name reservation (`crate::agent_trash`): an agent-identified
    /// caller tried to write, move or delete something under
    /// `<home>/agents/_trash/`, where removed employees are kept. Emptying an
    /// entry would release the reserved name; moving one back would restore an
    /// employee without the administrator. The path is deliberately not echoed
    /// in the message.
    BlockedRemovedAgentArea {
        caller: String,
        attempted_path: PathBuf,
    },
    /// Removed-name reservation, CLI lane: an agent-identified caller ran
    /// `duduclaw agent create <name>` (via Bash) for a name that is reserved
    /// because an employee of that name was removed, or the reservation could
    /// not be checked.
    BlockedReservedAgentName {
        caller: String,
        name: String,
    },
    /// G1 (2026-10): an agent-identified (or untrusted) caller tried to
    /// write, move or delete something under `<home>` outside its own agent
    /// directory and the shared `attachments/` fallback — the audit log
    /// (`tool_calls.jsonl`, the evidence grounding, the judge digest and the
    /// recent-actions feed read), the eval suites and held-out sets under
    /// `evals/`, every SQLite store, breaker state, licences, the shared
    /// wiki and global skills. See
    /// [`crate::org_field_guard::check_caller_scope`].
    BlockedHomeStateWrite {
        caller: String,
        attempted_path: PathBuf,
    },
    /// G1 / C (2026-10): the write would change a section or key of the
    /// caller's own `agent.toml` that a security gate reads (`[permissions]`,
    /// `[container]`, `[budget]`, `[runtime]`, `[evolution]`, …, `[prompt]
    /// cli_bare_mode`, `[model] account_pool`, `[agent] role`). Agent
    /// callers only; see `org_field_guard::rules::AGENT_SECURITY_SECTIONS`.
    BlockedAgentSecuritySection {
        file_name: String,
        attempted_path: PathBuf,
        changed: Vec<String>,
    },
    /// G1 round 3: where the write would really land could not be
    /// established — a dangling or looping symbolic link, an unreadable
    /// parent directory, a relative path with no known working directory, or
    /// a hook that cannot tell which DuDuClaw home it guards. Fail closed.
    BlockedUnresolvablePath {
        attempted_path: PathBuf,
        reason: String,
    },
    /// P2-B (C-1): an agent-identified (or untrusted) caller ran an
    /// operator-only memory command through Bash — `duduclaw memory
    /// forget-source …` or `duduclaw memory migrate-namespace …` (any
    /// subcommand, `list` included), also as `duduclaw-pro`.
    BlockedOperatorMemoryCommand {
        caller: String,
        command: String,
    },
}

impl GuardDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            Self::Allow | Self::NotAgentFile | Self::AllowedAgentWrite
        )
    }

    /// Format a user-facing block message suitable for surfacing through
    /// Claude Code's hook stderr (which the agent sees in-conversation).
    pub fn block_message(&self) -> Option<String> {
        match self {
            Self::BlockedOutsideHome { file_name, attempted_path } => Some(format!(
                "Blocked: refusing to write agent-structure file '{}' outside DuDuClaw home.\n\
                 Attempted path: {}\n\
                 Agents must be created via the `create_agent` MCP tool. \
                 Do not use Write/Edit to scaffold agents at arbitrary locations — \
                 the dashboard and registry only recognise agents under ~/.duduclaw/agents/<name>/.",
                file_name,
                attempted_path.display()
            )),
            Self::BlockedOrgFieldChange { attempted_path, changed, .. } => Some(format!(
                "已封鎖：組織欄位（name/reports_to/department）不可直接修改，請透過 agent_update 或儀表板調整。\n\
                 檔案：{}\n\
                 偵測到的變更：{}\n\
                 原因：這些欄位是委派授權的判定依據，允許 agent 自行改寫等同自助提權。",
                attempted_path.display(),
                changed.join("；")
            )),
            Self::BlockedProtectedField { attempted_path, changed, .. } => Some(format!(
                "已封鎖：權限設定（agent.toml 的 [capabilities] 段）不可直接修改，請透過 agent_update 或儀表板調整。\n\
                 檔案：{}\n\
                 偵測到的變更：{}\n\
                 原因：這一段定義了這位 AI 員工能用哪些工具、能不能操作電腦／瀏覽器／資料庫，\
                 允許執行中的程序自行改寫等同自助提權。",
                attempted_path.display(),
                changed.join("；")
            )),
            Self::BlockedProtectedSection { attempted_path, changed, .. } => Some(format!(
                "已封鎖：委派設定（config.toml 的 [delegation] / [acp] 段）不可直接修改，請透過儀表板或由管理者調整。\n\
                 檔案：{}\n\
                 偵測到的變更：{}",
                attempted_path.display(),
                changed.join("；")
            )),
            Self::BlockedUnverifiable { file_name, attempted_path, reason } => Some(format!(
                "已封鎖：無法驗證這次對 {} 的寫入內容，為保護組織／委派設定一律拒絕。\n\
                 檔案：{}\n\
                 原因：{}",
                file_name,
                attempted_path.display(),
                reason
            )),
            Self::BlockedBashProtectedWrite { file_name, verb } => Some(format!(
                "已封鎖：偵測到可能改寫 {} 的 shell 指令（含 `{}`）。\n\
                 組織欄位（name/reports_to/department）、權限設定（[capabilities]）、\
                 委派設定與身分設定不可直接修改，請透過 agent_update 或儀表板調整。",
                file_name, verb
            )),
            Self::BlockedIdentitySurface { file_name, attempted_path, reason } => Some(format!(
                "已封鎖：{} 屬於身分與保護機制設定，不可直接修改。\n\
                 檔案：{}\n\
                 原因：{}",
                file_name,
                attempted_path.display(),
                reason
            )),
            Self::BlockedForeignAgentDir { caller, owner, attempted_path } => Some(format!(
                "已封鎖：不能修改其他 AI 員工的檔案（{}）。\n\
                 檔案：{}\n\
                 你的身分：{}\n\
                 每位 AI 員工只能修改自己的資料夾；需要對方調整，請改用委派請對方處理，\
                 或由管理者從儀表板調整。",
                owner,
                attempted_path.display(),
                caller
            )),
            Self::BlockedUntrustedCaller { caller, attempted_path } => Some(format!(
                "已封鎖：身分驗證未通過（已啟用嚴格模式 require_identity_token），\
                 無法確認你是哪一位 AI 員工，因此暫停所有對 AI 員工資料夾的寫入。\n\
                 檔案：{}\n\
                 宣稱的身分：{}\n\
                 請聯絡管理者確認身分設定（identity.key / .mcp.json）是否正確。",
                attempted_path.display(),
                caller
            )),
            Self::BlockedHomeConfigWrite { caller, attempted_path } => Some(format!(
                "已封鎖：系統設定檔 config.toml 不可由 AI 員工直接修改，\
                 請透過儀表板或由管理者調整。\n\
                 檔案：{}\n\
                 你的身分：{}",
                attempted_path.display(),
                caller
            )),
            Self::BlockedOwnSoulWrite { caller, attempted_path } => Some(format!(
                "已封鎖：SOUL.md 是人格設定檔，AI 員工不可直接改寫（即使是自己的）。\n\
                 檔案：{}\n\
                 你的身分：{}\n\
                 人格設定一律由管理者透過儀表板調整；若管理者已為此 agent 開放自我修改\
                 （agent.toml 的 [permissions] can_modify_own_soul = true），\
                 請改呼叫 agent_update_soul 這個 MCP 工具，而不是直接寫檔。",
                attempted_path.display(),
                caller
            )),
            Self::BlockedOwnContractWrite { caller, attempted_path } => Some(format!(
                "已封鎖：CONTRACT.toml 是管理者為 AI 員工設定的行為界線，AI 員工不可直接改寫（即使是自己的）。\n\
                 檔案：{}\n\
                 你的身分：{}\n\
                 如果需要調整界線，請告知管理者，由管理者在儀表板修改這份契約。",
                attempted_path.display(),
                caller
            )),
            Self::BlockedRemovedAgentArea { caller, .. } => Some(format!(
                "已封鎖：已移除的 AI 員工由管理者保管，AI 員工不可改寫、移動或刪除。\n\
                 你的身分：{caller}\n\
                 需要復原或清除已移除的員工，請由管理者在儀表板處理。"
            )),
            Self::BlockedReservedAgentName { caller, name } => Some(format!(
                "已封鎖：無法以「{name}」建立 AI 員工，先前有一位同名的員工已被移除，這個名稱目前保留中\
                 （或目前無法確認）。\n\
                 你的身分：{caller}\n\
                 要重新使用這個名稱，請由管理者在儀表板處理；或改用其他名稱，透過 create_agent 建立。"
            )),
            Self::BlockedHomeStateWrite { caller, attempted_path } => Some(format!(
                "已封鎖：DuDuClaw 資料目錄裡的稽核紀錄、評測集、資料庫與系統狀態不可由 AI 員工直接寫入、移動或刪除。\n\
                 檔案：{}\n\
                 你的身分：{caller}\n\
                 你可以寫入自己的資料夾（agents/{caller}/）或共用附件資料夾（attachments/）；\
                 其他資料請透過對應的 MCP 工具處理（例如 wiki_write、tasks_update），或請管理者從儀表板調整。",
                attempted_path.display()
            )),
            Self::BlockedUnresolvablePath { attempted_path, reason } => Some(format!(
                "已封鎖：無法確認這次寫入實際會落在哪裡，為避免繞過資料夾保護一律拒絕。\n\
                 檔案：{}\n\
                 原因：{reason}",
                attempted_path.display()
            )),
            Self::BlockedAgentSecuritySection { attempted_path, changed, .. } => Some(format!(
                "已封鎖：agent.toml 中與權限、沙箱、預算、審核或身分有關的設定不可由 AI 員工直接修改，\
                 請由管理者透過儀表板調整。\n\
                 檔案：{}\n\
                 偵測到的變更：{}\n\
                 原因：這些設定由安全檢查讀取，允許執行中的程序自行改寫等同自助放寬限制。",
                attempted_path.display(),
                changed.join("；")
            )),
            Self::BlockedOperatorMemoryCommand { caller, command } => Some(format!(
                "已封鎖：`duduclaw memory {command}` 只能由管理者在自己的終端機執行，\
                 AI 員工不能執行（包含只列出內容的子指令）。\n\
                 你的身分：{caller}\n\
                 需要忘記或搬移記憶，請告訴管理者；刪除還需要管理員在儀表板核准。"
            )),
            _ => None,
        }
    }
}

/// Check whether a Write / Edit / MultiEdit `file_path` is permitted.
///
/// # Policy
/// - If `file_path`'s basename is not in [`AGENT_STRUCTURE_FILES`] → `NotAgentFile`
/// - If it *is* and the path lives under `<home>/agents/<name>/...` → `AllowedAgentWrite`
/// - Otherwise → `BlockedOutsideHome`
///
/// The `file_path` is lexically normalized (resolves `..` / `.` / repeated
/// separators) without touching the filesystem, so the guard works even
/// when the target file does not yet exist. Symlinks are **not** resolved
/// (the agent has no control over symlinks on the host, so following them
/// would only create TOCTOU risk without blocking any realistic attack).
///
/// `home` is typically `<user_home>/.duduclaw` (`DUDUCLAW_HOME` env var).
pub fn check_agent_file_write(file_path: &Path, home: &Path) -> GuardDecision {
    let Some(file_name) = file_path.file_name().and_then(|n| n.to_str()) else {
        return GuardDecision::NotAgentFile;
    };

    if !AGENT_STRUCTURE_FILES.contains(&file_name) {
        return GuardDecision::NotAgentFile;
    }

    let normalized = lexical_normalize(file_path);
    let agents_root = lexical_normalize(&home.join("agents"));

    // Must be strictly under <home>/agents/<some-name>/...
    //
    // Using components lets us avoid a false positive where a file path
    // is *equal to* `<home>/agents/` itself (which has no <name> segment),
    // and also avoids being fooled by sibling paths like `<home>/agentsX/`.
    //
    // L19 fix: `Path::starts_with` is case-sensitive, which wrongly *rejects*
    // a legitimate write on a case-insensitive filesystem (macOS / Windows)
    // when the configured `home` and the actual `file_path` differ only in
    // case (e.g. `/Users/Alice/...` vs `/users/alice/...`). Use a
    // case-insensitive component comparison instead.
    if let Some(suffix_len) = strip_prefix_ci(&normalized, &agents_root) {
        // Need at least one component (the agent name) after `agents/`,
        // plus the file basename — so >= 2 components total.
        if suffix_len >= 2 {
            return GuardDecision::AllowedAgentWrite;
        }
    }

    GuardDecision::BlockedOutsideHome {
        file_name: file_name.to_string(),
        attempted_path: normalized,
    }
}

/// Check whether a `Bash` tool command is permitted.
///
/// This is the Bash-tool analogue of [`check_agent_file_write`]. Agents can
/// otherwise bypass the Write/Edit guard by running shell commands like:
///
/// ```text
/// mkdir -p /some/project/.claude/agents/foo
/// cat > /some/project/.claude/agents/foo/agent.toml
/// cp template.toml /some/project/.claude/agents/foo/agent.toml
/// ```
///
/// # Policy
///
/// We reject any command whose text contains the substring `.claude/agents/`
/// anywhere. Rationale:
///
/// - The **canonical** agent root is `<home>/agents/<name>/` — it never
///   contains a `.claude/agents/` path segment, so the presence of this
///   substring is always suspicious.
/// - Each canonical agent has a `<home>/agents/<name>/.claude/` subdirectory
///   (for hooks/settings), but it contains `hooks/`/`settings.json`, never
///   a nested `agents/` — so `.claude/agents/` never appears in a legitimate
///   write path.
/// - Projects that the agent *works on* (e.g. a cloned git repo) should
///   never have an in-tree `.claude/agents/` — Claude Code's own config
///   lives at `~/.claude/`, not inside arbitrary project trees.
///
/// This is a conservative heuristic: any Bash command that even *mentions*
/// this path segment is blocked, including read-only listings. False
/// positives are acceptable — the agent can use the `Read` tool directly
/// or the `list_agents` MCP tool instead.
pub fn check_bash_command(command: &str, _home: &Path) -> GuardDecision {
    const SENTINEL: &str = ".claude/agents/";

    // L19 fix: match the sentinel robustly. The previous `command.find(...)`
    // matched only forward slashes and was case-sensitive, so
    // `.claude\agents\` (Windows separators) or `.CLAUDE/Agents/`
    // (case-insensitive filesystems) slipped through. Normalize a scratch
    // copy — backslashes → forward slashes, lowercased — and locate the
    // sentinel there. Since this transform is 1:1 on byte length, the match
    // offset maps directly back onto the original `command` for the
    // human-readable "attempted path" extraction below.
    let normalized: String = command
        .chars()
        .map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() })
        .collect();

    // Fast path — no sentinel anywhere means nothing to inspect.
    let Some(idx) = normalized.find(SENTINEL) else {
        return GuardDecision::NotAgentFile;
    };

    // Try to recover a readable "attempted path" for the error message.
    // Walk backwards from the match start to the nearest whitespace or
    // shell quote so the user sees something like
    // `/project/.claude/agents/foo` instead of a random slice.
    let prefix = &command[..idx];
    let path_start = prefix
        .rfind(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ';' | '&' | '|' | '(' | ')' | '='))
        .map(|p| p + 1)
        .unwrap_or(0);
    let suffix = &command[idx..];
    let path_end_rel = suffix
        .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ';' | '&' | '|' | '(' | ')'))
        .unwrap_or(suffix.len());
    let attempted = &command[path_start..idx + path_end_rel];

    GuardDecision::BlockedOutsideHome {
        file_name: SENTINEL.trim_end_matches('/').to_string(),
        attempted_path: PathBuf::from(attempted),
    }
}

/// Case-insensitive component-wise prefix check.
///
/// Returns `Some(suffix_component_count)` when `path` is `prefix` followed by
/// zero or more components, comparing each component case-insensitively
/// (matching how macOS/Windows filesystems treat paths). Returns `None` when
/// `path` is not under `prefix`. The leading root/prefix components are
/// matched verbatim via `eq_ignore_ascii_case` on their lossy string form.
fn strip_prefix_ci(path: &Path, prefix: &Path) -> Option<usize> {
    let mut path_comps = path.components();
    for pc in prefix.components() {
        let Some(actual) = path_comps.next() else {
            return None;
        };
        if !pc
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&actual.as_os_str().to_string_lossy())
        {
            return None;
        }
    }
    Some(path_comps.count())
}

/// Lexical path normalization — resolves `.`, `..`, and duplicate separators
/// without touching the filesystem. Does **not** follow symlinks or require
/// the path to exist.
///
/// Extracted as a helper so guard tests can exercise it independently.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                // Don't pop past the root/prefix.
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir)
                        | Some(Component::Prefix(_))
                        | None
                ) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from("/Users/alice/.duduclaw")
    }

    #[test]
    fn write_to_canonical_agent_dir_is_allowed() {
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/agent.toml");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn write_to_nested_path_in_canonical_dir_is_allowed() {
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/subteam/SOUL.md");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn write_to_project_dir_is_blocked() {
        let p = PathBuf::from("/Users/alice/Project/agents/tl-xianwen/SOUL.md");
        let decision = check_agent_file_write(&p, &home());
        match decision {
            GuardDecision::BlockedOutsideHome { file_name, .. } => {
                assert_eq!(file_name, "SOUL.md");
            }
            other => panic!("expected BlockedOutsideHome, got {other:?}"),
        }
    }

    #[test]
    fn write_to_sibling_agentsx_is_blocked() {
        // `/Users/alice/.duduclaw/agentsX/foo/SOUL.md` looks similar but is
        // not under `/Users/alice/.duduclaw/agents/`.
        let p = PathBuf::from("/Users/alice/.duduclaw/agentsX/foo/SOUL.md");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn write_to_agents_root_without_name_is_blocked() {
        // Directly writing <home>/agents/SOUL.md — missing the <name> segment.
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/SOUL.md");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn non_agent_files_are_not_our_concern() {
        let p = PathBuf::from("/Users/alice/Project/DuDuClaw/src/main.rs");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn mcp_json_in_canonical_dir_is_allowed() {
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/.mcp.json");
        assert_eq!(
            check_agent_file_write(&p, &home()),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn mcp_json_outside_is_blocked() {
        let p = PathBuf::from("/Users/alice/Project/x/.mcp.json");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn relative_path_with_parent_traversal_is_resolved() {
        // Edit tool can be called with a relative path from the agent's cwd.
        // After normalization it must still land inside the canonical dir
        // or be blocked.
        let p = PathBuf::from("/Users/alice/.duduclaw/agents/mybot/../../../../evil/agent.toml");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn contract_toml_is_covered() {
        let p = PathBuf::from("/Users/alice/Project/agents/x/CONTRACT.toml");
        assert!(matches!(
            check_agent_file_write(&p, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn block_message_contains_create_agent_hint() {
        let decision = GuardDecision::BlockedOutsideHome {
            file_name: "agent.toml".to_string(),
            attempted_path: PathBuf::from("/tmp/x/agent.toml"),
        };
        let msg = decision.block_message().unwrap();
        assert!(msg.contains("create_agent"));
        assert!(msg.contains("agent.toml"));
        assert!(msg.contains("/tmp/x/agent.toml"));
    }

    #[test]
    fn guard_decision_is_allowed_classification() {
        assert!(GuardDecision::Allow.is_allowed());
        assert!(GuardDecision::NotAgentFile.is_allowed());
        assert!(GuardDecision::AllowedAgentWrite.is_allowed());
        assert!(!GuardDecision::BlockedOutsideHome {
            file_name: "x".to_string(),
            attempted_path: PathBuf::from("/x"),
        }
        .is_allowed());
    }

    #[test]
    fn lexical_normalize_handles_dot_and_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/./c/../d")),
            PathBuf::from("/a/b/d")
        );
    }

    #[test]
    fn lexical_normalize_does_not_escape_root() {
        assert_eq!(
            lexical_normalize(Path::new("/../../x")),
            PathBuf::from("/x")
        );
    }

    // ── Bash command guard ─────────────────────────────────────────

    #[test]
    fn bash_mkdir_in_foreign_project_is_blocked() {
        let cmd = "mkdir -p /Users/lizhixu/Project/xianwen-online/.claude/agents/pm";
        let decision = check_bash_command(cmd, &home());
        match decision {
            GuardDecision::BlockedOutsideHome { attempted_path, .. } => {
                assert!(
                    attempted_path
                        .to_string_lossy()
                        .contains(".claude/agents/")
                );
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn bash_write_to_agent_toml_via_heredoc_is_blocked() {
        let cmd = "cat > /tmp/proj/.claude/agents/foo/agent.toml <<EOF\nname='x'\nEOF";
        assert!(matches!(
            check_bash_command(cmd, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn bash_with_quoted_path_is_blocked() {
        let cmd = r#"cp template.toml "/a b/.claude/agents/x/agent.toml""#;
        assert!(matches!(
            check_bash_command(cmd, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn bash_ls_mentioning_sentinel_is_also_blocked() {
        // Conservative: even read-only listings that mention `.claude/agents/`
        // are blocked. Agents should use `list_agents` MCP tool instead.
        let cmd = "ls /project/.claude/agents/";
        assert!(matches!(
            check_bash_command(cmd, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn bash_git_status_is_allowed() {
        let cmd = "git status --short";
        assert_eq!(
            check_bash_command(cmd, &home()),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_ls_canonical_agent_dotclaude_is_allowed() {
        // `<home>/agents/<name>/.claude/` is legitimate (hooks/settings)
        // and does NOT contain the `.claude/agents/` sentinel, so it passes.
        let cmd = "ls /Users/alice/.duduclaw/agents/agnes/.claude/settings.json";
        assert_eq!(
            check_bash_command(cmd, &home()),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_touching_claude_hooks_subdir_is_allowed() {
        // Writing into `.claude/hooks/` is fine — only `.claude/agents/`
        // triggers the guard.
        let cmd = "mkdir -p /project/.claude/hooks";
        assert_eq!(
            check_bash_command(cmd, &home()),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_nested_agents_under_home_is_still_blocked() {
        // Even under `<home>/agents/<name>/.claude/agents/` — that would be
        // a nested parallel hierarchy and is wrong.
        let cmd = "mkdir -p /Users/alice/.duduclaw/agents/agnes/.claude/agents/bad";
        assert!(matches!(
            check_bash_command(cmd, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    // ── L19: separator / case variants ──────────────────────────────

    #[test]
    fn bash_with_windows_backslash_separators_is_blocked() {
        // Windows-style backslashes must not bypass the sentinel.
        let cmd = r"mkdir C:\Users\bob\project\.claude\agents\evil";
        assert!(matches!(
            check_bash_command(cmd, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn bash_with_mixed_case_sentinel_is_blocked() {
        // Case-insensitive filesystems: `.CLAUDE/Agents/` is the same path.
        let cmd = "mkdir -p /project/.CLAUDE/Agents/evil";
        assert!(matches!(
            check_bash_command(cmd, &home()),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }

    #[test]
    fn bash_backslash_attempted_path_is_recovered() {
        // The human-readable attempted path is still extracted correctly even
        // when the original used backslashes (offsets are length-preserving).
        let cmd = r"cp t.toml C:\proj\.claude\agents\x\agent.toml";
        match check_bash_command(cmd, &home()) {
            GuardDecision::BlockedOutsideHome { attempted_path, .. } => {
                assert!(attempted_path.to_string_lossy().contains(".claude"));
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn file_write_with_mixed_case_home_is_allowed() {
        // On a case-insensitive FS the configured home and the actual path may
        // differ only in case; the legitimate write must still be allowed.
        let home = PathBuf::from("/Users/Alice/.duduclaw");
        let p = PathBuf::from("/users/alice/.duduclaw/agents/agnes/agent.toml");
        assert_eq!(
            check_agent_file_write(&p, &home),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn file_write_sibling_dir_still_blocked_case_insensitive() {
        // `agentsX` must not be mistaken for `agents` even case-insensitively.
        let home = PathBuf::from("/Users/Alice/.duduclaw");
        let p = PathBuf::from("/users/alice/.duduclaw/agentsX/agnes/agent.toml");
        assert!(matches!(
            check_agent_file_write(&p, &home),
            GuardDecision::BlockedOutsideHome { .. }
        ));
    }
}
