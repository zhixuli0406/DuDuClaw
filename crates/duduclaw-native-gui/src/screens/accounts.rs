// S5b1-A (2026-08-21) — "帳戶與登入" (`/app/system/accounts`, `nav.rs` id
// `accounts`). Visual authority: `commercial/design/duduclaw-s5-settings-
// pages/Accounts.dc.html` — a B6/B8 hybrid (already flagged as a deliberate
// canvas deviation by the canvas's own `Main.dc.html` cover sheet: this
// page's actual content is an AI-provider account rotation pool, not the
// B8-prescribed "device sign-in method" — see that file's "與 web 版面的刻意
// 差異" §1). Header → CLI 登入狀態 boxed-list → 帳號輪替清單 (ranked cards) →
// 本月用量總覽 footer bar, in that order.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed) ─────────────────────────────────────────────
//   `accounts.cli_credentials` (`handle_accounts_cli_credentials` ~L13078,
//     dispatch gate `require_manager!()` ~L5696) → `{ "credentials": [
//     { "runtime", "store", "installed", "present", "modified_at" } ] }`.
//     `cli_credential_statuses()` (`cli_auth.rs` ~L388) enumerates EXACTLY
//     `[Codex, Gemini, Grok]` — Claude and Antigravity have no comparable
//     "success file" check, so they are NEVER present in this list (pinned
//     by that module's own `cli_credential_statuses_cover_store_persisting_
//     runtimes` test). `present` is presence-only (a credential file
//     exists), not a live validity check.
//   `accounts.list` (`handle_accounts_list` ~L13050, dispatch gate
//     `require_admin!()` ~L5685) → `{ "accounts": [ { "id", "auth_method",
//     "priority", "is_healthy", "spent_this_month", "monthly_budget_cents",
//     "total_requests", "is_available", "label", "email", "subscription",
//     "expires_at", "days_until_expiry" } ] }`. `spent_this_month` and
//     `monthly_budget_cents` are both integer CENTS (`account_rotator.rs`
//     ~L650: `acc.spent_this_month += cost_cents`).
//   `accounts.budget_summary` (`handle_budget_summary` ~L13093, dispatch
//     gate `require_manager!()` ~L5689) → `{ "total_budget_cents",
//     "total_spent_cents", "accounts": [...] }` — same shape
//     `dashboard.rs`'s own budget card already consumes.
//
// ── Deliberate degradations, documented rather than faked ────────────────
// 1. The canvas's rotation-card "被 N 位員工使用中" line has NO backing
//    field anywhere in `AccountStatus`/`accounts.list` — no agent↔account
//    association is exposed by this RPC. Rather than invent a number, this
//    page drops that line entirely and shows what the payload actually
//    carries (label/email/subscription + budget bar), same "honest gap, not
//    a fabricated value" rule `dashboard.rs`'s own `parse_tasks_card` doc
//    comment already applies to a similar canvas/data mismatch.
// 2. The canvas's CLI-status "登入" button (for a not-yet-signed-in CLI) has
//    no wiring here — driving the real interactive `auth.cli_login.*` PTY
//    flow (start/input/status/cancel/finalize) is out of this page's scope
//    this round. Same "an inert control is honest; a silently-no-op click
//    handler is a worse lie" precedent `agents_detail.rs`'s read-only
//    capability rows establish — this page renders a plain status label,
//    never a clickable button, for CLI rows.
// 3. "新增帳號" (`accounts.add`, which needs a full multi-field credential
//    form) is rendered inert with a "即將推出"-style note rather than a
//    half-built form — same precedent as (2).
//
// State lives behind `gpui::Global` — same pattern `dashboard.rs`/`about.rs`
// already establish (no `main.rs` field, no shared session-scoped struct).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const CONTENT_MAX_WIDTH: f32 = 860.0;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct CliCredential {
    pub runtime: String,
    pub installed: bool,
    pub present: bool,
}

#[derive(Clone)]
pub struct RotationAccount {
    pub id: String,
    pub label: String,
    pub auth_method: String,
    pub priority: u32,
    pub is_healthy: bool,
    pub is_available: bool,
    pub spent_this_month_cents: i64,
    pub monthly_budget_cents: i64,
}

#[derive(Clone, Default)]
pub struct BudgetTotals {
    pub spent_cents: i64,
    pub budget_cents: i64,
}

pub struct AccountsState {
    requested: bool,
    pub cli_credentials: Loadable<Vec<CliCredential>>,
    pub rotation: Loadable<Vec<RotationAccount>>,
    pub budget: Loadable<BudgetTotals>,
}

impl Default for AccountsState {
    fn default() -> Self {
        Self {
            requested: false,
            cli_credentials: Loadable::Loading,
            rotation: Loadable::Loading,
            budget: Loadable::Loading,
        }
    }
}

impl Global for AccountsState {}

// ── Response parsing ──────────────────────────────────────────────────

fn parse_cli_credentials(v: &Value) -> Vec<CliCredential> {
    v.get("credentials")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| CliCredential {
            runtime: c.get("runtime").and_then(Value::as_str).unwrap_or("").to_string(),
            installed: c.get("installed").and_then(Value::as_bool).unwrap_or(false),
            present: c.get("present").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect()
}

/// Sorted by `priority` ascending (1 = used first) — the canvas's own
/// numbered-badge ordering.
fn parse_rotation_accounts(v: &Value) -> Vec<RotationAccount> {
    let mut accounts: Vec<RotationAccount> = v
        .get("accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|a| RotationAccount {
            id: a.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            label: a.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
            auth_method: a.get("auth_method").and_then(Value::as_str).unwrap_or("").to_string(),
            priority: a.get("priority").and_then(Value::as_u64).unwrap_or(0) as u32,
            is_healthy: a.get("is_healthy").and_then(Value::as_bool).unwrap_or(false),
            is_available: a.get("is_available").and_then(Value::as_bool).unwrap_or(false),
            spent_this_month_cents: a.get("spent_this_month").and_then(Value::as_i64).unwrap_or(0),
            monthly_budget_cents: a.get("monthly_budget_cents").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect();
    accounts.sort_by_key(|a| a.priority);
    accounts
}

fn parse_budget_totals(v: &Value) -> BudgetTotals {
    BudgetTotals {
        spent_cents: v.get("total_spent_cents").and_then(Value::as_i64).unwrap_or(0),
        budget_cents: v.get("total_budget_cents").and_then(Value::as_i64).unwrap_or(0),
    }
}

// ── Fetch orchestration ───────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "accounts" || cx.default_global::<AccountsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<AccountsState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx.clone(), "accounts.cli_credentials", json!({}), |cx, result| {
        cx.default_global::<AccountsState>().cli_credentials = result.map(|v| parse_cli_credentials(&v)).into();
    });
    spawn_call(cx, tx.clone(), "accounts.list", json!({}), |cx, result| {
        cx.default_global::<AccountsState>().rotation = result.map(|v| parse_rotation_accounts(&v)).into();
    });
    spawn_call(cx, tx, "accounts.budget_summary", json!({}), |cx, result| {
        cx.default_global::<AccountsState>().budget = result.map(|v| parse_budget_totals(&v)).into();
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
        CallError::Rejected(v) => v
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.as_str().map(str::to_string))
            .unwrap_or_else(|| v.to_string()),
    }
}

// ── Display helpers ────────────────────────────────────────────────────

/// Integer cents → "N,NNN" (no decimals) — same recipe
/// `dashboard_cards.rs::format_dollars` already establishes; duplicated
/// locally rather than widening that private helper's visibility (same
/// "local copy over widened visibility" precedent `agents_detail.rs`'s own
/// doc comment documents for this crate).
fn format_dollars(cents: i64) -> String {
    let dollars = cents / 100;
    let sign = if dollars < 0 { "-" } else { "" };
    let digits = dollars.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{}", grouped.chars().rev().collect::<String>())
}

fn cli_runtime_label(runtime: &str) -> &'static str {
    match runtime {
        "codex" => "Codex CLI",
        "gemini" => "Gemini CLI",
        "grok" => "Grok CLI",
        _ => "CLI",
    }
}

// ── Section: CLI 登入狀態 ─────────────────────────────────────────────

fn cli_status_dot(present: bool) -> Div {
    div()
        .size(px(7.))
        .rounded_full()
        .bg(theme::alpha(if present { theme::SUCCESS } else { theme::MUTED_FOREGROUND }, 1.0))
}

fn cli_credential_row(locale: Locale, cred: &CliCredential, is_last: bool) -> Div {
    let status_text = if !cred.installed {
        i18n::t(locale, "accounts.cli.notInstalled")
    } else if cred.present {
        i18n::t(locale, "accounts.cli.present")
    } else {
        i18n::t(locale, "accounts.cli.absent")
    };
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(cli_status_dot(cred.present))
                .child(cli_runtime_label(&cred.runtime)),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(status_text),
        );
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn cli_status_section(locale: Locale, state: &Loadable<Vec<CliCredential>>) -> Div {
    let body: Div = match state {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_2().p_4();
            for _ in 0..3 {
                wrap = wrap.child(skeleton(px(180.), px(14.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div()
            .p_4()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(creds) if creds.is_empty() => div()
            .p_4()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "accounts.cli.empty")),
        Loadable::Ready(creds) => {
            let mut wrap = div().flex().flex_col();
            for (idx, cred) in creds.iter().enumerate() {
                wrap = wrap.child(cli_credential_row(locale, cred, idx == creds.len() - 1));
            }
            wrap
        }
    };
    boxed_section(i18n::t(locale, "accounts.section.cli"), None, body)
        .child(div().px_4().pb_3().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.85)).child(i18n::t(locale, "accounts.cli.note")))
}

// ── Section: 帳號輪替清單 ─────────────────────────────────────────────

fn rank_badge(priority: u32, is_top: bool) -> Div {
    div()
        .size(px(22.))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_SM))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .when(is_top, |el| el.bg(theme::alpha(theme::BRAND, 0.16)).text_color(theme::alpha(theme::BRAND, 1.0)))
        .when(!is_top, |el| el.bg(theme::alpha(theme::MUTED, 1.0)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)))
        .child(priority.to_string())
}

fn auth_method_badge(auth_method: &str) -> Div {
    let label = if auth_method.eq_ignore_ascii_case("oauth") { "OAuth" } else { "API Key" };
    badge(label, BadgeVariant::Secondary)
}

fn budget_line(locale: Locale, account: &RotationAccount) -> Option<Div> {
    if account.monthly_budget_cents <= 0 {
        return None;
    }
    let pct = ((account.spent_this_month_cents as f64 / account.monthly_budget_cents as f64) * 100.0)
        .clamp(0.0, 100.0);
    Some(
        div()
            .mt_1()
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(i18n::tn(
                        locale,
                        "accounts.rotation.usedOfBudget",
                        &[
                            ("spent", &format_dollars(account.spent_this_month_cents)),
                            ("total", &format_dollars(account.monthly_budget_cents)),
                        ],
                    )),
            )
            .child(
                div()
                    .mt_1()
                    .h(px(5.))
                    .w(px(200.))
                    .rounded_full()
                    .bg(theme::alpha(theme::MUTED, 1.0))
                    .overflow_hidden()
                    .child(div().h_full().w(gpui::relative((pct / 100.0) as f32)).rounded_full().bg(theme::alpha(theme::BRAND, 1.0))),
            ),
    )
}

/// Three states, not two — matches the canvas's own distinction between a
/// currently-in-use healthy account ("正常", filled dot) and a healthy
/// account the rotator simply isn't using right now ("待命", no dot; the
/// canvas's rank-3 card). `is_available` (from `AccountStatus::is_available`
/// — cooldown/budget-exhaustion aware, distinct from the raw `is_healthy`
/// flag) is what tells the two apart.
fn rotation_status(locale: Locale, account: &RotationAccount) -> (Option<Div>, SharedString) {
    if !account.is_healthy {
        let dot = div().size(px(7.)).rounded_full().bg(theme::alpha(theme::DESTRUCTIVE, 1.0));
        return (Some(dot), i18n::t(locale, "accounts.rotation.unhealthy"));
    }
    if account.is_available {
        let dot = div().size(px(7.)).rounded_full().bg(theme::alpha(theme::SUCCESS, 1.0));
        (Some(dot), i18n::t(locale, "accounts.rotation.healthy"))
    } else {
        (None, i18n::t(locale, "accounts.rotation.standby"))
    }
}

fn rotation_account_row(locale: Locale, account: &RotationAccount, is_top: bool) -> Div {
    let (health_dot, health_text) = rotation_status(locale, account);
    let name = if account.label.trim().is_empty() { account.id.clone() } else { account.label.clone() };

    div()
        .flex()
        .items_center()
        .gap_3()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(rank_badge(account.priority, is_top))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(13.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(SharedString::from(name)),
                        )
                        .child(auth_method_badge(&account.auth_method)),
                )
                .children(budget_line(locale, account)),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1p5()
                .children(health_dot)
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(health_text)),
        )
}

fn rotation_section(locale: Locale, state: &Loadable<Vec<RotationAccount>>) -> Div {
    let header = div()
        .flex()
        .items_baseline()
        .gap_2()
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "accounts.section.rotation")),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.75))
                .child(i18n::t(locale, "accounts.rotation.hint")),
        );

    let body: Div = match state {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_2();
            for _ in 0..2 {
                wrap = wrap.child(skeleton(px(400.), px(56.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div().child(empty_state(
            "⚠️",
            i18n::t1(locale, "native.home.card.errorPrefix", "message", msg),
            None,
            None::<Div>,
        )),
        Loadable::Ready(accounts) if accounts.is_empty() => div().child(empty_state(
            "🔑",
            i18n::t(locale, "accounts.rotation.empty"),
            None,
            None::<Div>,
        )),
        Loadable::Ready(accounts) => {
            let mut wrap = div().flex().flex_col().gap_2();
            for account in accounts {
                wrap = wrap.child(rotation_account_row(locale, account, account.priority == 1));
            }
            wrap
        }
    };

    div().flex().flex_col().gap_2().child(header).child(body)
}

// ── Section: 本月用量總覽 ─────────────────────────────────────────────

fn budget_footer(locale: Locale, state: &Loadable<BudgetTotals>) -> Div {
    let value: Div = match state {
        Loadable::Loading => skeleton(px(140.), px(16.)),
        Loadable::Failed(msg) => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(totals) if totals.budget_cents <= 0 => div()
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(format!("NT$ {}", format_dollars(totals.spent_cents))),
        Loadable::Ready(totals) => div()
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .child(i18n::tn(
                locale,
                "accounts.budget.detail",
                &[
                    ("spent", &format_dollars(totals.spent_cents)),
                    ("total", &format_dollars(totals.budget_cents)),
                ],
            )),
    };

    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(18.))
        .py_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(12.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "accounts.budget.title")))
        .child(value)
}

// ── Shared boxed-section frame ────────────────────────────────────────

fn boxed_section(title: SharedString, trailing: Option<Div>, body: Div) -> Div {
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .pt_3p5()
        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(title))
        .children(trailing);
    div()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(header)
        .child(body)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;
    let g = cx.default_global::<AccountsState>();
    let cli_credentials = g.cli_credentials.clone();
    let rotation = g.rotation.clone();
    let budget = g.budget.clone();

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "nav.accounts")),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "nav.accounts.desc")),
                ),
        )
        // "新增帳號" — deliberately inert, see this file's header comment
        // point (3). No `on_click` handler at all.
        .child(
            div()
                .id("accounts-add-inert")
                .px_3p5()
                .h(px(32.))
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("+")
                .child(i18n::t(locale, "accounts.add")),
        );

    div()
        .id("accounts-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .p_6()
                .flex()
                .flex_col()
                .gap_4()
                .child(header)
                .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.75)).child(i18n::t(locale, "accounts.add.comingSoon")))
                .child(cli_status_section(locale, &cli_credentials))
                .child(rotation_section(locale, &rotation))
                .child(budget_footer(locale, &budget)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cli_credentials_reads_the_real_payload_shape() {
        let v = json!({ "credentials": [
            { "runtime": "codex", "store": "~/.codex/auth.json", "installed": true, "present": true, "modified_at": 1_700_000_000 },
            { "runtime": "gemini", "store": "~/.gemini/oauth.json", "installed": false, "present": false, "modified_at": null },
        ]});
        let creds = parse_cli_credentials(&v);
        assert_eq!(creds.len(), 2);
        assert_eq!(creds[0].runtime, "codex");
        assert!(creds[0].installed && creds[0].present);
        assert!(!creds[1].installed && !creds[1].present);
    }

    #[test]
    fn parse_cli_credentials_missing_array_is_empty_not_a_panic() {
        assert!(parse_cli_credentials(&json!({})).is_empty());
    }

    #[test]
    fn parse_rotation_accounts_sorts_by_priority_ascending() {
        let v = json!({ "accounts": [
            { "id": "b", "priority": 3 },
            { "id": "a", "priority": 1 },
            { "id": "c", "priority": 2 },
        ]});
        let accounts = parse_rotation_accounts(&v);
        assert_eq!(accounts.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["a", "c", "b"]);
    }

    #[test]
    fn parse_rotation_accounts_reads_cents_fields_verbatim() {
        let v = json!({ "accounts": [
            { "id": "a", "priority": 1, "spent_this_month": 372000, "monthly_budget_cents": 600000, "is_healthy": true, "auth_method": "oauth", "label": "Claude Max" },
        ]});
        let accounts = parse_rotation_accounts(&v);
        assert_eq!(accounts[0].spent_this_month_cents, 372000);
        assert_eq!(accounts[0].monthly_budget_cents, 600000);
        assert!(accounts[0].is_healthy);
        assert_eq!(accounts[0].auth_method, "oauth");
        assert_eq!(accounts[0].label, "Claude Max");
    }

    #[test]
    fn parse_budget_totals_reads_the_dashboard_shared_shape() {
        let v = json!({ "total_spent_cents": 372000, "total_budget_cents": 600000 });
        let totals = parse_budget_totals(&v);
        assert_eq!(totals.spent_cents, 372000);
        assert_eq!(totals.budget_cents, 600000);
    }

    #[test]
    fn format_dollars_groups_thousands() {
        assert_eq!(format_dollars(372000), "3,720");
        assert_eq!(format_dollars(600000), "6,000");
        assert_eq!(format_dollars(0), "0");
    }

    #[test]
    fn cli_runtime_label_maps_the_three_covered_runtimes_and_falls_back() {
        assert_eq!(cli_runtime_label("codex"), "Codex CLI");
        assert_eq!(cli_runtime_label("gemini"), "Gemini CLI");
        assert_eq!(cli_runtime_label("grok"), "Grok CLI");
        assert_eq!(cli_runtime_label("claude"), "CLI");
    }

    #[test]
    fn describe_call_error_prefers_structured_message_over_bare_string() {
        let msg = describe_call_error(&CallError::Rejected(json!({"code": "denied", "message": "權限不足"})));
        assert_eq!(msg, "權限不足");
        let msg2 = describe_call_error(&CallError::Rejected(json!("連線失敗")));
        assert_eq!(msg2, "連線失敗");
    }
}
