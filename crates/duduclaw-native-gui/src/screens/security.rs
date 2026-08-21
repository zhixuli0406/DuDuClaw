// WP-S6b1-K (S6b 第一波, 2026-08-21) — "安全" (`SecurityPage.dc.html`, B5+
// B18 合併版 — the QA #5 pass folded the earlier B5 batch's 憑證代理/掛載守衛/
// 稽核 content into this one artboard, see `commercial/design/duduclaw-s6-
// biz-pages/Main.dc.html`'s own "與 web 版面的刻意差異" §1/§3). A "進階設定"
// drill-down leaf (`active_page == "security"`, no `nav.rs` entry of its
// own — same convention every S5b1-C/S5b2-F drill-down leaf establishes),
// reached from `manage_advanced.rs`'s 安全 row (that file is a parallel
// WP's territory per this task's own "manage_advanced.rs 歸 L" boundary —
// this pass only self-attaches its own `shell.rs` branch).
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/SecurityPage.
// dc.html` — breadcrumb → header → 緊急控制 (3 toggle rows) → 4-stat grid →
// 憑證代理／掛載守衛 (two boxed columns) → 存取權限矩陣 (能力類別 tabs + 員工
// 授權清單) → 稽核 (兩條連結列).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `security.status` (dispatch ~L6160, handler `handle_security_status`
//   ~L20346, `require_admin!()`) → `{"credential_proxy":{"active","
//   vault_backend","injected_secrets"}, "mount_guard":{"rules":[{"path",
//   "access"}]}, "rbac":[{"agent_id","role","tool_use","web_access",
//   "file_write","shell_exec","delegate"}], "rate_limiter":{
//   "requests_per_minute","concurrent_requests"}, "soul_drift":[...]}`.
//   `soul_drift`/`delegate` are not drawn by this canvas and are not read
//   here. Backs every section below EXCEPT 緊急控制 and 稽核 (see the two
//   deviations below).
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. 緊急控制 (全域急停開關/唯讀模式/危險工具二次確認) — grepped this whole
//    workspace for a backing concept (`全域急停`/`readonly_mode`/
//    `emergency_stop`-as-a-config-toggle) and found none: no RPC, no
//    config field, no killswitch sub-field maps onto these three specific
//    switches (`killswitch.get`'s own triggers/circuit_breaker/safety_words
//    shape is a different, unrelated control surface). Per this task's own
//    brief ("緊急開關等決策類組裝不真按"), the three rows render exactly as
//    the canvas draws them (title/desc/switch position), but the switch
//    itself uses the SAME muted, non-interactive `static_toggle` shape
//    every other inert control on this page uses (see `governance.rs`'s
//    own `static_toggle` doc comment for the "always dimmed, never the
//    canvas's brand-blue 'on' look" rationale — copied here as a tiny local
//    fn rather than imported, since this page and `governance.rs` are a
//    different design-authority pairing than `governance.rs`/`wiki_trust.
//    rs`'s own GovernanceShell batch).
// 2. 稽核 section — the canvas draws two static LINK rows (稽核歷程 → /
//    安全審計 →), not a live event list (unlike `web/src/pages/SecurityPage.
//    tsx`'s own `AuditLogCard`). Both destinations (`logs`/`secaudit`) are
//    themselves still-inert rows on `manage_advanced.rs` as of this pass
//    (no `nav.rs`/`shell.rs` page exists yet for either) — this page
//    therefore renders the two rows decoratively, with no `on_click`,
//    matching canvas fidelity exactly rather than importing web's live-list
//    behavior or inventing a destination this pass doesn't own.
// 3. RBAC "最後變更" column — the canvas draws a per-row date; `security.
//    status`'s `rbac` entries carry no timestamp field at all (confirmed
//    reading `handle_security_status`'s full `rbac_entries` construction —
//    only `agent_id`/`role`/four booleans/`delegate`). Rather than drop the
//    column (losing canvas layout fidelity) or fabricate a date, every row
//    shows a literal "—" here, the same "honest unavailable, not invented"
//    placeholder `mcp_keys.rs::format_created_date` uses for an unparseable
//    date.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::dashboard::{error_row, Loadable};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct MountRule {
    pub path: String,
    pub access: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RbacRow {
    pub agent_id: String,
    pub role: String,
    pub tool_use: bool,
    pub web_access: bool,
    pub file_write: bool,
    pub shell_exec: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SecurityStatus {
    pub proxy_active: bool,
    pub vault_backend: String,
    pub injected_secrets: i64,
    pub mount_rules: Vec<MountRule>,
    pub rbac: Vec<RbacRow>,
    pub requests_per_minute: i64,
    pub concurrent_requests: i64,
}

pub fn parse_security_status(v: &Value) -> SecurityStatus {
    let cp = v.get("credential_proxy");
    let mount_rules = v
        .get("mount_guard")
        .and_then(|m| m.get("rules"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some(MountRule {
                        path: r.get("path")?.as_str()?.to_string(),
                        access: r.get("access").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let rbac = v
        .get("rbac")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some(RbacRow {
                        agent_id: r.get("agent_id")?.as_str()?.to_string(),
                        role: r.get("role").and_then(Value::as_str).unwrap_or("").to_string(),
                        tool_use: r.get("tool_use").and_then(Value::as_bool).unwrap_or(false),
                        web_access: r.get("web_access").and_then(Value::as_bool).unwrap_or(false),
                        file_write: r.get("file_write").and_then(Value::as_bool).unwrap_or(false),
                        shell_exec: r.get("shell_exec").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let rl = v.get("rate_limiter");
    SecurityStatus {
        proxy_active: cp.and_then(|c| c.get("active")).and_then(Value::as_bool).unwrap_or(false),
        vault_backend: cp.and_then(|c| c.get("vault_backend")).and_then(Value::as_str).unwrap_or("—").to_string(),
        injected_secrets: cp.and_then(|c| c.get("injected_secrets")).and_then(Value::as_i64).unwrap_or(0),
        mount_rules,
        rbac,
        requests_per_minute: rl.and_then(|r| r.get("requests_per_minute")).and_then(Value::as_i64).unwrap_or(60),
        concurrent_requests: rl.and_then(|r| r.get("concurrent_requests")).and_then(Value::as_i64).unwrap_or(5),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RbacCategory {
    ToolUse,
    WebAccess,
    FileWrite,
    ShellExec,
}

impl RbacCategory {
    const ALL: [RbacCategory; 4] = [RbacCategory::ToolUse, RbacCategory::WebAccess, RbacCategory::FileWrite, RbacCategory::ShellExec];

    fn id(self) -> &'static str {
        match self {
            RbacCategory::ToolUse => "toolUse",
            RbacCategory::WebAccess => "webAccess",
            RbacCategory::FileWrite => "fileWrite",
            RbacCategory::ShellExec => "shellExec",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            RbacCategory::ToolUse => "security.rbac.category.toolUse",
            RbacCategory::WebAccess => "security.rbac.category.webAccess",
            RbacCategory::FileWrite => "security.rbac.category.fileWrite",
            RbacCategory::ShellExec => "security.rbac.category.shellExec",
        }
    }

    fn allowed(self, row: &RbacRow) -> bool {
        match self {
            RbacCategory::ToolUse => row.tool_use,
            RbacCategory::WebAccess => row.web_access,
            RbacCategory::FileWrite => row.file_write,
            RbacCategory::ShellExec => row.shell_exec,
        }
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct SecurityState {
    requested: bool,
    pub status: Loadable<SecurityStatus>,
    pub category: RbacCategory,
}

impl SecurityState {
    fn new() -> Self {
        Self { requested: false, status: Loadable::Loading, category: RbacCategory::ToolUse }
    }
}

impl Global for SecurityState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<SecurityState>() {
        cx.set_global(SecurityState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<SecurityState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<SecurityState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "security.status", json!({}), |cx, result| {
        cx.global_mut::<SecurityState>().status = result.map(|v| parse_security_status(&v)).into();
    });
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Breadcrumb ("進階設定 › 安全", not shared with GovernanceShell) ───────

fn breadcrumb(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("security-breadcrumb")
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("security-breadcrumb-root")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "nav.manageAdvanced"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "manageAdvanced";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(i18n::t(locale, "security.breadcrumb")))
}

/// Non-interactive toggle pill for decision-class controls this pass
/// assembles but does not wire — see this file's own module doc comment
/// (deviation #1) for why this stays a small local copy of `governance.rs::
/// static_toggle` rather than an import.
fn static_toggle(knob_right: bool) -> Div {
    div()
        .relative()
        .w(px(36.))
        .h(px(21.))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::alpha(theme::MUTED, 0.6))
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(if knob_right { px(17.) } else { px(2.) })
                .size(px(17.))
                .rounded_full()
                .bg(theme::alpha(0xffffff, 0.9)),
        )
}

// ── Sections ───────────────────────────────────────────────────────────

fn section_label(locale: Locale, key: &'static str) -> Div {
    div()
        .px_0p5()
        .pb_1p5()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, key))
}

fn boxed(rows: Vec<Div>) -> Div {
    let mut group = div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border());
    for r in rows {
        group = group.child(r);
    }
    group
}

fn row_frame(is_last: bool) -> Div {
    let row = div().flex().items_center().gap_3().px_4().py_2p5();
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

/// 緊急控制 — three static rows; see module doc comment deviation #1. Knob
/// positions are the canvas's own literal states (off/off/on) since the
/// muted color already signals "not live", same reasoning `governance.rs::
/// policy_row`'s toggle applies.
fn emergency_section(locale: Locale) -> Div {
    let row = |title_key: &'static str, desc_key: &'static str, knob_right: bool, is_last: bool| {
        row_frame(is_last)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, title_key)))
                    .child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, desc_key))),
            )
            .child(static_toggle(knob_right))
    };
    div()
        .flex()
        .flex_col()
        .child(section_label(locale, "security.emergency.title"))
        .child(boxed(vec![
            row("security.emergency.globalStop.title", "security.emergency.globalStop.desc", false, false),
            row("security.emergency.readonly.title", "security.emergency.readonly.desc", false, false),
            row("security.emergency.confirmDanger.title", "security.emergency.confirmDanger.desc", true, true),
        ]))
}

fn kpi_grid(locale: Locale, status: &SecurityStatus) -> Div {
    let tile = |label_key: &'static str, value: String| {
        div()
            .flex_1()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .px_4()
            .py_3()
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, label_key)))
            .child(div().mt_1().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
    };
    div()
        .flex()
        .gap_2p5()
        .flex_wrap()
        .child(tile("security.reqPerMin", status.requests_per_minute.to_string()))
        .child(tile("security.concurrent", status.concurrent_requests.to_string()))
        .child(tile("security.injectedSecrets", status.injected_secrets.to_string()))
        .child(tile("security.mountGuard.count", status.mount_rules.len().to_string()))
}

fn credential_proxy_card(locale: Locale, status: &SecurityStatus) -> Div {
    let kv = |label_key: &'static str, value: Div, is_last: bool| {
        row_frame(is_last)
            .justify_between()
            .child(div().text_size(px(12.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, label_key)))
            .child(value)
    };
    let status_badge = if status.proxy_active {
        badge(i18n::t(locale, "security.active"), BadgeVariant::Success)
    } else {
        badge(i18n::t(locale, "security.inactive"), BadgeVariant::Outline)
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .child(section_label(locale, "security.credentialProxy.title"))
        .child(boxed(vec![
            kv("security.proxyStatus", status_badge, false),
            kv(
                "security.vaultBackend",
                div().font_family("SF Mono").text_size(px(12.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(status.vault_backend.clone())),
                true,
            ),
        ]))
}

fn mount_guard_card(locale: Locale, status: &SecurityStatus) -> Div {
    let access_label = |access: &str| -> (SharedString, u32) {
        match access {
            "rw" => (i18n::t(locale, "security.mountGuard.rw"), theme::SUCCESS),
            "ro" => (i18n::t(locale, "security.mountGuard.ro"), theme::WARNING),
            "deny" => (i18n::t(locale, "security.mountGuard.deny"), theme::DESTRUCTIVE),
            other => (other.to_string().into(), theme::MUTED_FOREGROUND),
        }
    };
    let n = status.mount_rules.len();
    let rows: Vec<Div> = if n == 0 {
        vec![row_frame(true).justify_center().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "common.noData")))]
    } else {
        status
            .mount_rules
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (label, color) = access_label(&r.access);
                row_frame(i + 1 == n)
                    .justify_between()
                    .child(div().font_family("SF Mono").text_size(px(12.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(r.path.clone())))
                    .child(div().text_size(px(11.5)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(color, 1.0)).child(label))
            })
            .collect()
    };
    div().flex_1().flex().flex_col().child(section_label(locale, "security.mountGuard.title")).child(boxed(rows))
}

fn rbac_category_chip(locale: Locale, cat: RbacCategory, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id(format!("security-rbac-cat-{}", cat.id()))
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        })
        .child(i18n::t(locale, cat.label_key()))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<SecurityState>().category = cat;
            cx.notify();
        }))
}

fn rbac_matrix_section(locale: Locale, status: &SecurityStatus, category: RbacCategory, cx: &mut Context<RootView>) -> Div {
    let mut chip_row = div().flex().gap_1p5();
    for cat in RbacCategory::ALL {
        chip_row = chip_row.child(rbac_category_chip(locale, cat, cat == category, cx));
    }

    let n = status.rbac.len();
    let table = if n == 0 {
        boxed(vec![row_frame(true).justify_center().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "common.noData")))])
    } else {
        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(theme::alpha(theme::MUTED, 0.35))
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(div().flex_1().child(i18n::t(locale, "security.rbac.agent")))
            .child(div().w(px(90.)).flex_shrink_0().child(i18n::t(locale, "security.rbac.lastChanged")))
            .child(div().w(px(50.)).flex_shrink_0().child(i18n::t(locale, "security.rbac.allowed")));
        let mut rows = vec![header];
        for (i, r) in status.rbac.iter().enumerate() {
            rows.push(
                row_frame(i + 1 == n)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(theme::TEXT_SM))
                            .child(SharedString::from(r.agent_id.clone()))
                            .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(format!("({})", r.role))),
                    )
                    .child(div().w(px(90.)).flex_shrink_0().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.6)).child("—"))
                    .child(div().w(px(50.)).flex_shrink_0().child(static_toggle(category.allowed(r)))),
            );
        }
        boxed(rows)
    };

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section_label(locale, "security.rbac.title"))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "security.rbac.hint"))),
        )
        .child(chip_row)
        .child(table)
}

/// 稽核 — two static link rows, see module doc comment deviation #2.
fn audit_section(locale: Locale) -> Div {
    let link_row = |title_key: &'static str, desc_key: &'static str, is_last: bool| {
        row_frame(is_last)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, title_key)))
                    .child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, desc_key))),
            )
            .child(SharedString::from("›"))
    };
    div()
        .flex()
        .flex_col()
        .child(section_label(locale, "security.audit.title"))
        .child(boxed(vec![
            link_row("security.audit.history.title", "security.audit.history.desc", false),
            link_row("security.audit.secaudit.title", "security.audit.secaudit.desc", true),
        ]))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let status = cx.global::<SecurityState>().status.clone();
    let category = cx.global::<SecurityState>().category;

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "security.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "security.subtitle")));

    let body: Div = match &status {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(90.))).child(skeleton(px(760.), px(120.))),
        Loadable::Failed(err) => div().child(error_row(locale, err)),
        Loadable::Ready(s) if s.rbac.is_empty() && s.mount_rules.is_empty() && s.requests_per_minute == 0 => {
            div().child(empty_state("🛡️", i18n::t(locale, "security.empty"), None, None::<Div>))
        }
        Loadable::Ready(s) => div()
            .flex()
            .flex_col()
            .gap_3p5()
            .child(kpi_grid(locale, s))
            .child(
                div()
                    .flex()
                    .gap_2p5()
                    .child(credential_proxy_card(locale, s))
                    .child(mount_guard_card(locale, s)),
            )
            .child(rbac_matrix_section(locale, s, category, cx))
            .child(audit_section(locale)),
    };

    div()
        .id("security-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(880.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .p_2()
                .child(breadcrumb(locale, cx))
                .child(header)
                .child(emergency_section(locale))
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_security_status_reads_real_handler_shape() {
        let v = json!({
            "credential_proxy": { "active": true, "vault_backend": "env", "injected_secrets": 9 },
            "mount_guard": { "rules": [ { "path": "~/.duduclaw/", "access": "deny" }, { "path": "~/workspace/", "access": "rw" } ] },
            "rbac": [ { "agent_id": "cs-bot", "role": "worker", "tool_use": true, "web_access": false, "file_write": true, "shell_exec": false, "delegate": false } ],
            "rate_limiter": { "requests_per_minute": 600, "concurrent_requests": 24 },
            "soul_drift": [],
        });
        let s = parse_security_status(&v);
        assert!(s.proxy_active);
        assert_eq!(s.vault_backend, "env");
        assert_eq!(s.injected_secrets, 9);
        assert_eq!(s.mount_rules.len(), 2);
        assert_eq!(s.mount_rules[0].access, "deny");
        assert_eq!(s.rbac.len(), 1);
        assert!(s.rbac[0].tool_use);
        assert!(!s.rbac[0].web_access);
        assert_eq!(s.requests_per_minute, 600);
        assert_eq!(s.concurrent_requests, 24);
    }

    #[test]
    fn parse_security_status_missing_object_defaults_gracefully() {
        let s = parse_security_status(&json!({}));
        assert!(!s.proxy_active);
        assert_eq!(s.vault_backend, "—");
        assert_eq!(s.injected_secrets, 0);
        assert!(s.mount_rules.is_empty());
        assert!(s.rbac.is_empty());
        assert_eq!(s.requests_per_minute, 60);
        assert_eq!(s.concurrent_requests, 5);
    }

    #[test]
    fn parse_security_status_skips_rbac_rows_without_agent_id() {
        let v = json!({ "rbac": [ { "role": "worker" } ] });
        assert!(parse_security_status(&v).rbac.is_empty());
    }

    #[test]
    fn rbac_category_allowed_maps_to_the_right_field() {
        let row = RbacRow { agent_id: "a".into(), role: "worker".into(), tool_use: true, web_access: false, file_write: true, shell_exec: false };
        assert!(RbacCategory::ToolUse.allowed(&row));
        assert!(!RbacCategory::WebAccess.allowed(&row));
        assert!(RbacCategory::FileWrite.allowed(&row));
        assert!(!RbacCategory::ShellExec.allowed(&row));
    }

    #[test]
    fn rbac_category_all_has_four_distinct_ids() {
        let ids: Vec<&str> = RbacCategory::ALL.iter().map(|c| c.id()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), 4);
        assert_eq!(sorted.len(), 4);
    }
}
