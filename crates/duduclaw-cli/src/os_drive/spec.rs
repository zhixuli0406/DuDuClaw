//! A7a — machine-readable metadata table backing `duduclaw os commands
//! --json`. See `commercial/docs/DESIGN-os-self-drive-2026-08.md` §4 for the
//! schema rationale (why a plain `const` array, not an inventory/macro
//! registry — 15 commands is small enough that a hand-written table is
//! easier to review than macro infrastructure, and the lint tests below give
//! back the "forgot to register a command" safety net a macro would have
//! bought).
//!
//! This table is deliberately independent of the `clap` `OsDisplayCommands`/
//! `OsSystemCommands`/`OsNetworkCommands` enums in `lib.rs` — clap enforces
//! "no duplicate command name within one level" and "every variant has a
//! doc comment" at compile time already; this table is the ADDITIONAL,
//! machine-readable surface an agent reads via `duduclaw os commands --json`
//! (§4 of the design doc — "this is the precondition A7b's skill needs").
//! Keeping the two in sync is enforced by code review + the count tripwire
//! test at the bottom of this file, not by generating one from the other —
//! see the design doc §10 for why a macro/derive approach was rejected for
//! this small a surface.

use serde_json::{json, Value};

/// One `duduclaw os <group> <verb>` command's full metadata.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Full route as a user would type it, e.g. `"os display cursor-size-get"`.
    pub route: &'static str,
    pub group: &'static str,
    pub verb: &'static str,
    pub summary: &'static str,
    /// Positional argument shape, empty string when the command takes none.
    pub args: &'static str,
    pub examples: &'static [&'static str],
    pub hidden: bool,
    /// NOT "requires-sudo" (Omarchy's own convention) — see design doc §5.
    /// `true` means: an operator terminal (no `DUDUCLAW_AGENT_ID` in the
    /// environment) still runs it directly, but an agent-identity caller
    /// must clear `ApprovalBroker` first.
    pub requires_approval: bool,
}

impl CommandSpec {
    fn to_json(&self) -> Value {
        json!({
            "route": self.route,
            "group": self.group,
            "verb": self.verb,
            "summary": self.summary,
            "args": self.args,
            "examples": self.examples,
            "hidden": self.hidden,
            "requires_approval": self.requires_approval,
        })
    }
}

/// The full command table. Order matches declaration order in `lib.rs`'s
/// `OsDisplayCommands`/`OsSystemCommands`/`OsNetworkCommands` enums (display,
/// then system, then network), plus the introspection command itself last.
pub const ALL_COMMANDS: &[CommandSpec] = &[
    // ── display (comp shell_control op reuse) ───────────────────────────
    CommandSpec {
        route: "os display cursor-size-get",
        group: "display",
        verb: "cursor-size-get",
        summary: "讀取目前人類滑鼠游標大小（comp shell_control get_cursor_source）。",
        args: "",
        examples: &["duduclaw os display cursor-size-get"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os display cursor-size-set",
        group: "display",
        verb: "cursor-size-set",
        summary: "設定人類滑鼠游標大小，封閉集 24/32/48/64/96（comp shell_control set_cursor_size）。",
        args: "<24|32|48|64|96>",
        examples: &["duduclaw os display cursor-size-set 48"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os display cursor-source-get",
        group: "display",
        verb: "cursor-source-get",
        summary: "讀取滑鼠游標圖案來源（system/brand）（comp shell_control get_cursor_source）。",
        args: "",
        examples: &["duduclaw os display cursor-source-get"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os display cursor-source-set",
        group: "display",
        verb: "cursor-source-set",
        summary: "設定滑鼠游標圖案來源，system 或 brand（comp shell_control set_cursor_source）。",
        args: "<system|brand>",
        examples: &["duduclaw os display cursor-source-set brand"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os display theme-set",
        group: "display",
        verb: "theme-set",
        summary: "即時切換 comp 自己的伺服器端裝飾（標題列/邊框/Alt-Tab）明暗主題，light 或 dark。\
                  這條線只有 set，沒有 get（comp 本身不持久化這個值，見設計文件 §3）。",
        args: "<light|dark>",
        examples: &["duduclaw os display theme-set dark"],
        hidden: false,
        requires_approval: false,
    },
    // ── system (gateway device_about / device_ops(sysd) reuse) ──────────
    CommandSpec {
        route: "os system about",
        group: "system",
        verb: "about",
        summary: "裝置身分：OS 版本/kernel/hostname/device id（gateway device_about::collect_device_about）。",
        args: "",
        examples: &["duduclaw os system about"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os system timezone-get",
        group: "system",
        verb: "timezone-get",
        summary: "讀取目前時區/本地時間/UTC 時間（gateway device_about::collect_timedate）。",
        args: "",
        examples: &["duduclaw os system timezone-get"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os system timezone-set",
        group: "system",
        verb: "timezone-set",
        summary: "設定系統時區，經 duduclaw-sysd。agent 身分呼叫需先過 ApprovalBroker 審批。",
        args: "<IANA 時區，如 Asia/Taipei>",
        examples: &["duduclaw os system timezone-set Asia/Taipei"],
        hidden: false,
        requires_approval: true,
    },
    CommandSpec {
        route: "os system ntp-get",
        group: "system",
        verb: "ntp-get",
        summary: "讀取 NTP 時間同步是否啟用/已同步（gateway device_about::collect_timedate）。",
        args: "",
        examples: &["duduclaw os system ntp-get"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os system ntp-set",
        group: "system",
        verb: "ntp-set",
        summary: "啟用/停用 NTP 時間同步，經 duduclaw-sysd。agent 身分呼叫需先過 ApprovalBroker 審批。",
        args: "<true|false>",
        examples: &["duduclaw os system ntp-set true"],
        hidden: false,
        requires_approval: true,
    },
    CommandSpec {
        route: "os system update-check",
        group: "system",
        verb: "update-check",
        summary: "檢查可用更新：duduclaw 本體自我更新 + appliance OS image 更新狀態\
                  （gateway updater::check_update + device_ops update_status）。",
        args: "",
        examples: &["duduclaw os system update-check"],
        hidden: false,
        requires_approval: false,
    },
    // ── network (gateway network module reuse, read-only) ───────────────
    CommandSpec {
        route: "os network status",
        group: "network",
        verb: "status",
        summary: "列出網路介面（gateway device::collect_network）。",
        args: "",
        examples: &["duduclaw os network status"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os network wired-status",
        group: "network",
        verb: "wired-status",
        summary: "有線網路介面狀態（gateway network::wired::collect_wired_status）。",
        args: "",
        examples: &["duduclaw os network wired-status"],
        hidden: false,
        requires_approval: false,
    },
    CommandSpec {
        route: "os network wifi-status",
        group: "network",
        verb: "wifi-status",
        summary: "Wi-Fi 連線/IP/網際網路可達性狀態（gateway network::status，需 appliance + iwd）。",
        args: "",
        examples: &["duduclaw os network wifi-status"],
        hidden: false,
        requires_approval: false,
    },
    // ── introspection ─────────────────────────────────────────────────
    CommandSpec {
        route: "os commands",
        group: "meta",
        verb: "commands",
        summary: "機器可讀能力清單（本命令自己）——agent 自我發現用 --json 讀出完整表格。",
        args: "[--json]",
        examples: &["duduclaw os commands", "duduclaw os commands --json"],
        hidden: false,
        requires_approval: false,
    },
];

/// The full `duduclaw os commands --json` payload.
pub fn commands_json() -> Value {
    json!({
        "commands": ALL_COMMANDS.iter().map(CommandSpec::to_json).collect::<Vec<_>>(),
    })
}

/// Human-readable rendering for `duduclaw os commands` (no `--json`).
pub fn render_commands_table() -> String {
    let mut out = String::new();
    out.push_str("DuDuClaw OS self-drive commands\n");
    out.push_str(&"=".repeat(40));
    out.push('\n');
    let mut last_group = "";
    for cmd in ALL_COMMANDS {
        if cmd.hidden {
            continue;
        }
        if cmd.group != last_group {
            out.push('\n');
            out.push_str(&format!("[{}]\n", cmd.group));
            last_group = cmd.group;
        }
        let approval = if cmd.requires_approval { " (requires-approval)" } else { "" };
        out.push_str(&format!("  {}{}\n    {}\n", cmd.route, approval, cmd.summary));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Tripwire: keep in sync with the actual OsDisplayCommands/
    // OsSystemCommands/OsNetworkCommands variant counts in lib.rs (5 + 6 + 3
    // + 1 introspection = 15) — the real enforcement of "every clap command
    // has metadata" is code review + this count; the exhaustive `match` in
    // `cmd_os_drive` (no wildcard arm) is what actually guarantees dispatch
    // coverage, this is a second, independent tripwire for the metadata side.
    #[test]
    fn command_count_matches_the_designed_surface() {
        assert_eq!(ALL_COMMANDS.len(), 15, "did you add/remove a verb without updating this table?");
    }

    #[test]
    fn every_command_has_a_non_empty_summary() {
        for cmd in ALL_COMMANDS {
            assert!(
                !cmd.summary.trim().is_empty(),
                "command {} has no summary — every command must document itself \
                 (Omarchy借鑑 #7 lint: 'a command without a summary is a command \
                 nobody can safely call')",
                cmd.route
            );
        }
    }

    #[test]
    fn every_route_is_globally_unique() {
        let mut seen = HashSet::new();
        for cmd in ALL_COMMANDS {
            assert!(seen.insert(cmd.route), "duplicate route: {}", cmd.route);
        }
        assert_eq!(seen.len(), ALL_COMMANDS.len());
    }

    #[test]
    fn route_is_exactly_group_and_verb_joined_by_a_space() {
        for cmd in ALL_COMMANDS {
            // The introspection command itself (`group: "meta"`) is a
            // top-level `duduclaw os commands` verb, not nested under a
            // clap subcommand group the way display/system/network are —
            // its route is deliberately just "os <verb>". Every real
            // group/verb command still gets the full "os <group> <verb>"
            // check.
            let expected =
                if cmd.group == "meta" { format!("os {}", cmd.verb) } else { format!("os {} {}", cmd.group, cmd.verb) };
            assert_eq!(
                cmd.route, expected,
                "route must be exactly \"os <group> <verb>\" (or \"os <verb>\" for the meta \
                 introspection command) — found {:?}, expected {expected:?}",
                cmd.route
            );
        }
    }

    #[test]
    fn requires_approval_is_only_set_on_the_two_designed_write_verbs() {
        // Design doc §5: only timezone-set/ntp-set are gated. Every other
        // command is either read-only or a reversible, non-destructive
        // human-preference write (display group) — a regression here (e.g.
        // someone flipping requires_approval on a read verb, or forgetting
        // it on a new write verb) is exactly the kind of drift this lint
        // exists to catch.
        let gated: Vec<&str> = ALL_COMMANDS
            .iter()
            .filter(|c| c.requires_approval)
            .map(|c| c.route)
            .collect();
        assert_eq!(gated, vec!["os system timezone-set", "os system ntp-set"]);
    }

    #[test]
    fn commands_json_round_trips_through_serde_json() {
        let v = commands_json();
        let arr = v["commands"].as_array().expect("commands must be a JSON array");
        assert_eq!(arr.len(), ALL_COMMANDS.len());
        // Spot-check one entry has every documented field.
        let first = &arr[0];
        for key in ["route", "group", "verb", "summary", "args", "examples", "hidden", "requires_approval"] {
            assert!(first.get(key).is_some(), "commands_json entry missing key {key:?}: {first}");
        }
    }

    #[test]
    fn render_commands_table_never_panics_and_lists_every_visible_route() {
        let text = render_commands_table();
        for cmd in ALL_COMMANDS.iter().filter(|c| !c.hidden) {
            assert!(text.contains(cmd.route), "table text missing route {}", cmd.route);
        }
    }
}
