// Fake data for the Home surface — Shell-S0 prototype convention (see
// `duduclaw-native-gui/src/nav.rs`'s "placeholder icon: an uppercase letter
// + a fixed accent color, real iconography deferred" precedent). The
// `glyph` fields below are no longer what actually draws in most of those
// slots: ICON-1 (2026-08-22) wired the approved boards' own stroke icons in
// via `crate::icons`, and each `glyph` is now the FALLBACK for its slot.
// Content is lifted verbatim (zh-TW copy, hex colors,
// counts) from the visual spec
// (`commercial/design/duduclaw-os-desktop/Main.dc.html`) — nothing here is
// wired to a live gateway/agent yet, matching the task brief's "假資料
// （內容照畫板文案，繁中硬編——S0 原型慣例）".
//
// Centralized in this one file so `home.rs` stays presentation-only and
// `main.rs` stays data-free — same separation `duduclaw-native-gui`'s own
// `screens::prototypes` module keeps between its static page data and
// rendering.
//
// ── WP-fix-QEMU-a (2026-09-05): that S0 convention stops at chrome/decor
// content, not at claims about THIS machine's own state or history ────────
// A QEMU first-boot walkthrough found `GREETING`/`GOAL_CARDS`/
// `ACTIVITY_SHELF`/`BATTERY_PCT`/`CLOCK` (a placeholder person's name, three
// invented in-flight tasks, a fabricated "completed today" log, an
// unconditional battery reading, and a frozen clock value) still rendering
// on a freshly installed system that had none of that. See the note just
// above `SUGGESTION_CHIPS` below for exactly what replaced each one and
// where. The dock's Launcher/Notifications/ControlCenter chrome strings
// further down are unaffected — that content is either genuinely generic
// (menu labels, button labels) or already real (approval cards, since
// Shell-S4/WP-S4-notif), never a claim like "已存到 工作/月報" about work
// that never happened.

// ── `DockApp` / `DOCK_APPS` were REMOVED in APP-1 (2026-08-22) ──────────
// They used to be this crate's whole "app registry": six hand-authored
// entries lifted from the design board (信箱/文件/瀏覽器/圖片/訊息/行事曆),
// five of which had no real app behind them. Both the dock and the
// Launcher's app-search rendered them, so on a real appliance the operator
// was reading a menu of software that was not installed — reported from the
// VM, and the reason this work package exists.
//
// What replaced them:
//   * the real inventory — `crate::apps::installed` (flatpak + XDG
//     `.desktop` enumeration), held in `crate::apps::feed::
//     InstalledAppsFeed`, rendered by `home/home_dock.rs` and
//     `overlay/launcher.rs`. Nothing falls back to canned entries when it
//     is empty or fails.
//   * the one non-fictional thing that array carried — a real flatpak ref
//     plus its remote, which an inventory cannot express for an app that is
//     not installed yet — now `crate::apps::catalog::INSTALL_CATALOG`, the
//     installable catalog behind the Launcher's separate 「可安裝」section.
//   * `VerifiedTier` moved to `crate::apps` (it is now a live lookup over
//     real apps, `crate::apps::catalog::verified_tier`, not a per-row
//     constant in a canned array).
//
// This file keeps only genuinely decorative board content — goal cards, the
// activity shelf, menu strings, the ControlCenter tiles. The app list is no
// longer part of it.

/// A pinned agent avatar in the dock (right of the app icons, after the
/// divider) — the design board's "杜"/"財" circles. The small corner status
/// dot is `running` (busy, matches its own avatar's brand hue) vs
/// `needs_human` (amber) per the task brief's "running 藍/needs_human 紅橘"
/// status convention (the design board's actual sample data has one of
/// each, not one of every state — kept verbatim rather than inventing a
/// third example).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDockStatus {
    Running,
    NeedsHuman,
}

/// Shell-S1 (2026-08-20, Home/overlay dark theme): which SEMANTIC
/// color family a status ring / badge belongs to. This file used to resolve
/// each badge's text/bg pair below straight to a
/// literal hex, appropriate when there was only ONE design board to lift
/// colors from. Now that Home/overlay has a light AND a dark board, the
/// concrete hex for each kind differs per theme (see `crate::palette::
/// ShellPalette`'s own header comment for the exact pairs) — resolving that
/// here would mean either importing the palette type (which owns gpui
/// `Rgba`/`Hsla` fields, breaking this module's own "stays gpui-free,
/// independently unit-testable" discipline, see this file's header comment)
/// or duplicating every literal twice per field. Storing the semantic KIND
/// instead and letting `ShellPalette::badge_accent`/`badge_bg`/`badge_text`
/// resolve the actual color at render time (`home/home_dock.rs`, `overlay/
/// notifications.rs`) keeps this file plain data while still being
/// theme-correct.
///
/// `AgentDockStatus`'s own status dot deliberately does NOT go through this
/// enum: `NeedsHuman`'s dot resolves through `ShellPalette::warning_dot`,
/// NOT `badge_accent(Warning)` — the small circular dot and the badge/ring
/// token are different fields in dark (see `warning_dot`'s own doc comment
/// on `ShellPalette`), so forcing `AgentDockStatus` through this 3-member
/// vocabulary would need a dead `Success` match arm for no actual gain;
/// `home/home_dock.rs::dock_agent` matches on `AgentDockStatus` directly
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    Warning,
    Success,
    Brand,
}


// ── `GoalCard`/`GOAL_CARDS`, `GREETING`, `ACTIVITY_SHELF`, `BATTERY_PCT`,
// `CLOCK` were REMOVED in WP-fix-QEMU-a (2026-09-05, a QEMU walkthrough
// defect fix) ─────────────────────────────────────────────────────────────
// A first-boot QEMU walkthrough of a freshly installed, never-used system
// showed 「晚上好，Louis」 as the greeting, three canned goal cards (每日營
// 收日報自動化／官網改版文案／客服月報) and a "今日" activity strip claiming
// specific completed work — all invented, on a machine that had done
// nothing and belonged to nobody named Louis (the real operator, set during
// OOBE/live-install account creation, is read from `oobe::boot_operator_name`
// and shown correctly on the SAME boot's lock screen — this file's old
// `GREETING` constant was the one surface that ignored it). Same shape as
// APP-1's `DOCK_APPS` deletion (see this file's own note below): a design
// board mockup shipped as literal running-system content that could not be
// true for an appliance nobody had used yet.
//
// What replaced them, all in `home.rs`/`home/home_dock.rs`:
//   * the greeting — `home.rs::greeting_text`, a real time-of-day
//     salutation (`chrono::Local::now()`, the same clock source
//     `lockscreen::render::now_local_strings` already established) plus the
//     real operator display name threaded down from `ShellView.operator_name`
//     (the SAME field the lock screen's own identity row reads) — greets
//     without a name when none is on file, never a placeholder person.
//   * the goal cards — `home/home_dock.rs::goal_cards_row`, built from the
//     real `overlay::notifications_feed::NotificationsFeed` (pending
//     approvals → "等你決定" cards) and `overlay::task_progress_feed::
//     TaskProgressFeed` (in-progress tasks → "進行中" cards), the same two
//     feeds A4 (2026-08-24) already wired into the dock's own badge. An
//     honest empty-state line renders instead when neither feed has
//     anything real to show, and a distinct offline line when the gateway
//     is unreachable.
//   * the "今日" activity strip — `home/home_dock.rs::activity_shelf`, built
//     from `NotificationsFeed::decided()` (approvals the operator has
//     actually decided this session) — real, if narrower in scope than the
//     old fabricated agent-activity lines (no "小杜 完成 X" agent-work log
//     exists yet on the client side to draw from honestly). Renders nothing
//     at all when there is no real decided-today activity, same "an empty
//     heading over an empty list is noise" rule `overlay::
//     notifications_tasks::task_progress_section` already applies to its own
//     section, rather than a second fabricated "nothing happened today"
//     line duplicating the goal cards' own empty state.
//   * the battery reading (`BATTERY_PCT`) — `crate::battery::
//     battery_percent()`, a real read of this machine's own
//     `/sys/class/power_supply` tree; hidden entirely (not "0%") on a
//     desktop appliance with no battery hardware, which `apps::installed`'s
//     own "Absent, not an error" convention already establishes for exactly
//     this situation (a dev Mac / a battery-less appliance is the correct,
//     quiet answer).
//   * the menu-bar clock (`CLOCK`) — `home.rs`'s own `chrono::Local::now()`
//     read, ticking on the same 20s cadence `lockscreen::render::
//     schedule_clock_tick` already uses for the identical purpose, so the
//     two clocks on one machine can no longer read different times (the
//     other half of the QEMU defect: menu bar showed 22:58 while the lock
//     screen's real local-time clock showed 17:16 on the same boot).
//
// `SUGGESTION_CHIPS` below stays: the task brief's own ruling is that
// generic example prompts ("整理 Q3 報表" etc. — things ANY operator could
// plausibly type, not a claim about what has already happened) are fine to
// keep static, unlike a card or a greeting asserting a specific fact.

// `APPROVAL_TICKER` (the old hardcoded menu-bar ticker text) was removed in
// Shell-S4 (2026-08-22, WP-S4-notif) — `home.rs::ticker_text` now renders
// the real `overlay::notifications_feed::NotificationsFeed`'s pending list
// instead of one static example sentence.
pub const COMPOSER_PLACEHOLDER: &str = "交代一件事給你的 AI 團隊…";
pub const MENU_BRAND: &str = "DuDuClaw";

pub const MENU_ITEMS: &[&str] = &["檔案", "編輯", "顯示", "AI 團隊", "視窗"];
pub const SUGGESTION_CHIPS: &[&str] = &["整理 Q3 報表", "清理收件匣", "排明天的行程"];

// ── Launcher overlay fake data ────────────────────────────────────────────
// Content lifted verbatim from `commercial/design/duduclaw-os-desktop/
// Launcher.dc.html` — see `overlay/launcher.rs`'s header comment for layout.

/// One "檔案" result row — icon has no background box in the design board
/// (just a colored glyph), unlike the app-result rows above.
pub struct LauncherFileResult {
    pub id: &'static str,
    pub glyph: &'static str,
    pub glyph_hex: u32,
    pub label: &'static str,
    pub meta: &'static str,
}

pub const LAUNCHER_FILE_RESULTS: &[LauncherFileResult] = &[
    LauncherFileResult { id: "launcher-file-folder", glyph: "夾", glyph_hex: 0x54a8ef, label: "財務/請款/2026-08/", meta: "資料夾" },
    LauncherFileResult { id: "launcher-file-xlsx", glyph: "表", glyph_hex: 0x21a366, label: "請款單-0819.xlsx", meta: "今天 14:20" },
];

// `LAUNCHER_QUERY` (the old static predisplay query text) was removed in
// WP-A3 (2026-08-22) — `overlay/launcher.rs::query_row` now renders the
// real, live-typed `OverlayUiState.launcher_query` instead.
pub const LAUNCHER_SECTION_DELEGATE: &str = "交辦";
pub const LAUNCHER_SECTION_APPS: &str = "應用程式";
pub const LAUNCHER_SECTION_FILES: &str = "檔案";
pub const LAUNCHER_DELEGATE_AGENT_BG_HEX: u32 = 0x0f766e;
pub const LAUNCHER_FOOTER_LEFT: &str = "↑↓ 選擇 · Enter 執行 · Tab 換分類";
/// Names the binding that ACTUALLY summons the Launcher — `cmd-k` in
/// `main.rs`'s keymap (gpui maps `cmd` to the Logo/Super key on Linux) and
/// comp's global Super+K intent, both of which the menu-bar pill already
/// labels「⌘K」and `docs/features/51-os-keyboard-shortcuts.md` documents as
/// Cmd+K. The design board's original copy said「Super 鍵隨時喚起」, which
/// promised a lone-Super tap that no layer of the stack has ever bound
/// (verified on the appliance VM 2026-09-08: a Super tap does nothing,
/// Super+K and the pill both open it) — a false promise, not a placeholder.
pub const LAUNCHER_FOOTER_RIGHT: &str = "⌘K 隨時喚起";

// ── Notifications overlay fake data ───────────────────────────────────────
// Content lifted verbatim from `commercial/design/duduclaw-os-desktop/
// Notifications.dc.html` — see `overlay/notifications.rs`'s header comment.
//
// Shell-S4 (2026-08-22, WP-S4-notif): the approval CARDS themselves stopped
// being fake data this round — they now come from the real gateway
// (`gateway_client::list_approvals`, rendered via `overlay::
// notifications_feed::ApprovalRow`), so the old `ApprovalCard`/
// `APPROVAL_CARDS` (two hardcoded example approvals) were removed rather
// than left as unreferenced dead code. `NOTIF_APPROVE_LABEL`/
// `NOTIF_REJECT_LABEL` below replace the old per-card `approve_label`/
// `reject_label` pair (which differed per card in the design board itself,
// "核准"/"駁回" vs "批准"/"先不要") with ONE generic pair: the real gateway
// has no per-approval button-label field to carry that distinction, so a
// single consistent label is the honest choice rather than inventing one.

pub const NOTIF_APPROVE_LABEL: &str = "核准";
pub const NOTIF_REJECT_LABEL: &str = "駁回";

/// Which avatar an activity row shows — an agent's initial-in-a-circle
/// (same convention as `DockAgent`) or the system's own gradient glyph (the
/// update-available row has no agent behind it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAvatar {
    Agent { initial: &'static str, bg_hex: u32 },
    System,
}



pub const NOTIF_HEADER_TITLE: &str = "通知";
pub const NOTIF_MARK_ALL_READ: &str = "全部標為已讀";
pub const NOTIF_FOOTER: &str = "已在儀表板處理過的項目會自動消失";
pub const NOTIF_NOTE_PLACEHOLDER: &str = "補一句備註…";
pub const NOTIF_APPROVED_LABEL: &str = "已核准";
pub const NOTIF_REJECTED_LABEL: &str = "已駁回";

// ── ControlCenter overlay fake data ───────────────────────────────────────
// Content lifted verbatim from `commercial/design/duduclaw-os-desktop/
// ControlCenter.dc.html` — see `overlay/controlcenter.rs`'s header comment.
// The AI-team switch DEFAULTS (自動化/主動行為 on, 全部暫停 off) are
// RUNTIME state, not fake data — they live in `overlay::OverlayUiState`
// instead, seeded from this same board's snapshot.

pub struct QuickTile {
    pub id: &'static str,
    pub glyph: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub active: bool,
}

pub const QUICK_TILES: &[QuickTile] = &[
    QuickTile { id: "tile-wifi", glyph: "W", title: "Wi-Fi", subtitle: "DuDu-Office", active: true },
    QuickTile { id: "tile-bluetooth", glyph: "B", title: "藍牙", subtitle: "關閉", active: false },
    QuickTile { id: "tile-dnd", glyph: "勿", title: "勿擾", subtitle: "關閉", active: false },
];

/// A static quick-settings slider (volume/brightness) — `pct` is the fill
/// fraction (0.0–1.0), matched verbatim from `ControlCenter.dc.html`'s
/// inline `width: 62%` / `width: 80%`. Non-interactive this round (task
/// brief scoped "開關做視覺 toggle 狀態" to the AI-team switches only, not
/// these sliders — see `overlay/controlcenter.rs`'s header comment).
pub struct SliderRow {
    pub glyph: &'static str,
    pub pct: f32,
}

pub const SLIDER_ROWS: &[SliderRow] = &[SliderRow { glyph: "音", pct: 0.62 }, SliderRow { glyph: "光", pct: 0.80 }];

pub const CC_SECTION_AI_TEAM: &str = "AI 團隊";
pub const CC_SWITCH_AUTOMATION_LABEL: &str = "自動化";
pub const CC_SWITCH_AUTOMATION_DESC: &str = "例行工作與目標迴圈照常執行";
pub const CC_SWITCH_PROACTIVE_LABEL: &str = "主動行為";
pub const CC_SWITCH_PROACTIVE_DESC: &str = "允許 AI 員工主動提出建議與提醒";
pub const CC_SWITCH_PAUSE_ALL_LABEL: &str = "全部暫停";
pub const CC_SWITCH_PAUSE_ALL_DESC: &str = "一鍵暫停所有 AI 行為，待辦保留";
pub const CC_FOOTER_LINK: &str = "打開管理面";
/// Shell-S4-lock (2026-08-22): the manual-lock entry point in ControlCenter's
/// footer — see `overlay/controlcenter.rs::lock_button`'s own doc comment.
/// Plain zh-TW literal, same "chrome content stays hardcoded" convention
/// every other `CC_*` string in this block already follows (Home/overlay's
/// established non-i18n boundary — see `crate::i18n`'s own header comment).
pub const CC_LOCK_BUTTON: &str = "鎖定";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_chips_has_three_entries() {
        assert_eq!(SUGGESTION_CHIPS.len(), 3);
    }

    #[test]
    fn menu_items_has_five_entries() {
        assert_eq!(MENU_ITEMS.len(), 5);
    }

    #[test]
    fn agent_dock_status_has_exactly_the_two_documented_variants() {
        // `AgentDockStatus` no longer carries its own `BadgeKind` mapping
        // (see that enum's own doc comment for why — `home/home_dock.rs::
        // dock_agent` matches on these variants directly instead) — this
        // file's remaining stake in the status/color relationship is just
        // that the two variants themselves stay exactly Running/NeedsHuman,
        // pinned via `Debug` formatting so a future rename or a third
        // variant fails loudly here rather than silently in the renderer.
        assert_eq!(format!("{:?}", AgentDockStatus::Running), "Running");
        assert_eq!(format!("{:?}", AgentDockStatus::NeedsHuman), "NeedsHuman");
    }

    // ── Launcher ─────────────────────────────────────────────────────────

    #[test]
    fn launcher_footer_names_the_real_summon_binding() {
        // The footer must advertise the binding the shell actually has
        // (`cmd-k` / Super+K, shown as「⌘K」on the menu-bar pill) — never a
        // lone Super tap, which nothing binds. See the const's own doc.
        assert!(LAUNCHER_FOOTER_RIGHT.contains("⌘K"), "footer must name ⌘K: {LAUNCHER_FOOTER_RIGHT}");
        assert!(!LAUNCHER_FOOTER_RIGHT.contains("Super 鍵"), "a lone Super tap is not bound anywhere");
    }

    #[test]
    fn launcher_file_results_has_two_entries_matching_the_design_board() {
        assert_eq!(LAUNCHER_FILE_RESULTS.len(), 2);
    }

    #[test]
    fn launcher_file_result_ids_are_unique() {
        let mut ids: Vec<&str> = LAUNCHER_FILE_RESULTS.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), LAUNCHER_FILE_RESULTS.len());
    }

    #[test]
    fn quick_tiles_has_three_entries_matching_the_design_board() {
        assert_eq!(QUICK_TILES.len(), 3);
    }

    #[test]
    fn quick_tile_ids_are_unique() {
        let mut ids: Vec<&str> = QUICK_TILES.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), QUICK_TILES.len());
    }

    #[test]
    fn slider_rows_has_two_entries_matching_the_design_board() {
        assert_eq!(SLIDER_ROWS.len(), 2);
    }

    #[test]
    fn slider_pcts_are_within_the_unit_range() {
        for row in SLIDER_ROWS {
            assert!((0.0..=1.0).contains(&row.pct), "pct {} out of range for {}", row.pct, row.glyph);
        }
    }
}
