// Tab render functions for the "設定" rail group's second half (大腦/預算/
// 自動化/進階) — sibling of `edit_agent.rs`/`edit_agent_data.rs`/
// `edit_agent_tabs_a.rs`. See `edit_agent.rs`'s module doc comment for the
// page-wide scope decision and RPC citations.
//
// `brain_tab` is the ONE tab with drawn visual authority (`commercial/
// design/duduclaw-s6-form-pages/EditAgent.dc.html`'s 大腦 example — 模型/
// 執行引擎/帳號池/輔助模型, four boxed-list sections) — this function
// matches that canvas layout section-for-section. The other three tabs
// (預算/自動化/進階) have no canvas artwork; each is a direct port of the
// matching `web/src/pages/agent-form/EditAgentPage.tsx` tab, same "誠實偏差"
// commenting convention `edit_agent_tabs_a.rs` establishes for every field
// this page cannot honestly source a live value for.

use gpui::{div, prelude::*, px, Div, SharedString};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, BadgeVariant};
use crate::screens::edit_agent::{boxed_group, bool_badge, kv_row, kv_row_desc, plain_value, section};
use crate::screens::edit_agent_data::{format_dollars, format_hours, format_percent, EditAgentDetail};
use crate::theme;

/// `runtime.provider`/`runtime.fallback` raw tokens → a human label — no
/// existing reusable mapper in this crate covers this (checked
/// `screens::accounts`/`screens::prototypes::p12_agent_detail`, neither
/// exports one), so this is a fresh, small, honest mapping: falls back to
/// the raw token itself for any value not in this crate's known runtime
/// provider set, never a blank/guessed label.
fn runtime_provider_label(locale: Locale, provider: &str) -> SharedString {
    let key = match provider {
        "claude" => "native.editAgent.runtime.claude",
        "codex" => "native.editAgent.runtime.codex",
        "gemini" => "native.editAgent.runtime.gemini",
        "agy" | "antigravity" => "native.editAgent.runtime.antigravity",
        "openai-compat" | "openai_compat" => "native.editAgent.runtime.openaiCompat",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

fn expressiveness_label(locale: Locale, value: &str) -> SharedString {
    let key = match value {
        "minimal" => "native.editAgent.expressiveness.minimal",
        "moderate" => "native.editAgent.expressiveness.moderate",
        "expressive" => "native.editAgent.expressiveness.expressive",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

// ── 大腦 (canvas-drawn) ──────────────────────────────────────────────────

pub(super) fn brain_tab(locale: Locale, detail: &EditAgentDetail) -> Div {
    let unset = || i18n::t(locale, "native.editAgent.unset");
    let preferred = detail.model_preferred.clone().map(SharedString::from).unwrap_or_else(unset);
    let fallback = detail.model_fallback.clone().map(SharedString::from).unwrap_or_else(unset);
    let api_mode = detail.model_api_mode.clone().map(SharedString::from).unwrap_or_else(unset);

    let model_section = section(
        i18n::t(locale, "native.editAgent.section.model"),
        boxed_group(vec![
            kv_row_desc(
                i18n::t(locale, "native.editAgent.row.preferredModel"),
                i18n::t(locale, "native.editAgent.row.preferredModelDesc"),
                plain_value(preferred),
            ),
            kv_row(i18n::t(locale, "native.editAgent.row.fallbackModel"), plain_value(fallback)),
            kv_row(i18n::t(locale, "native.editAgent.row.apiMode"), plain_value(api_mode)),
        ]),
    );

    let runtime_env = detail.runtime_provider.clone().map(|p| runtime_provider_label(locale, &p)).unwrap_or_else(|| i18n::t(locale, "native.editAgent.runtimeAuto"));
    let runtime_fallback_val = detail.runtime_fallback.clone().map(|p| runtime_provider_label(locale, &p)).unwrap_or_else(unset);
    let pty_pool_value: Div = match detail.pty_pool_enabled {
        Some(on) => bool_badge(locale, on),
        None => plain_value(unset()),
    };
    let runtime_section = section(
        i18n::t(locale, "native.editAgent.section.runtime"),
        boxed_group(vec![
            kv_row_desc(
                i18n::t(locale, "native.editAgent.row.runtimeProvider"),
                i18n::t(locale, "native.editAgent.row.runtimeProviderDesc"),
                plain_value(runtime_env),
            ),
            kv_row(i18n::t(locale, "native.editAgent.row.runtimeFallback"), plain_value(runtime_fallback_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.ptyPool"), pty_pool_value),
        ]),
    );

    // Canvas copy for the empty state: "留白＝可用所有健康帳號，過濾在健康
    // 檢查之後套用" — `account_pool` is a raw comma-separated string on the
    // wire (`agent.toml [model] account_pool`), so this splits+trims client
    // side rather than expecting the server to have already tokenized it.
    let pool_value: Div = match detail.account_pool.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let mut wrap = div().flex().flex_wrap().gap_1();
            for tok in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                wrap = wrap.child(badge(SharedString::from(tok.to_string()), BadgeVariant::Secondary));
            }
            wrap
        }
        None => plain_value(i18n::t(locale, "native.editAgent.accountPoolEmpty")),
    };
    let pool_section = section(
        i18n::t(locale, "native.editAgent.section.accountPool"),
        boxed_group(vec![kv_row_desc(
            i18n::t(locale, "native.editAgent.row.availableAccounts"),
            i18n::t(locale, "native.editAgent.availableAccountsDesc"),
            pool_value,
        )]),
    );

    // 誠實偏差: canvas 畫的「輔助模型／裁決·摘要用模型」對應 web 表單的
    // `utility` 欄位（`claude-haiku-4-6` 只是畫布上的示意值）——那也是純寫
    // 入欄位，`agents.inspect` 完全沒有回讀路徑。本頁不臆造「目前值」去湊
    // 畫布，改用一行誠實說明。
    let utility_section = section(
        i18n::t(locale, "native.editAgent.section.utilityModel"),
        boxed_group(vec![div()
            .px_3p5()
            .py_3()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.editAgent.utilityModelNote"))]),
    );

    div().flex().flex_col().gap_3p5().child(model_section).child(runtime_section).child(pool_section).child(utility_section)
}

// ── 預算 ───────────────────────────────────────────────────────────────

pub(super) fn budget_tab(locale: Locale, detail: &EditAgentDetail) -> Div {
    let limit = i18n::t1(locale, "native.editAgent.currencyValue", "amount", &format_dollars(detail.budget_monthly_limit_cents));
    let spent = i18n::t1(locale, "native.editAgent.currencyValue", "amount", &format_dollars(detail.budget_spent_cents));
    div().flex().flex_col().gap_3p5().child(section(
        i18n::t(locale, "native.editAgent.section.budget"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.monthlyLimit"), plain_value(limit)),
            kv_row(i18n::t(locale, "native.editAgent.row.spent"), plain_value(spent)),
            kv_row(
                i18n::t(locale, "native.editAgent.row.warnThreshold"),
                plain_value(SharedString::from(format!("{}%", detail.budget_warn_threshold_percent))),
            ),
            kv_row(i18n::t(locale, "native.editAgent.row.hardStop"), bool_badge(locale, detail.budget_hard_stop)),
        ]),
    ))
}

// ── 自動化 ─────────────────────────────────────────────────────────────

pub(super) fn automation_tab(locale: Locale, detail: &EditAgentDetail) -> Div {
    let unset = || i18n::t(locale, "native.editAgent.unset");

    let heartbeat_section = section(
        i18n::t(locale, "native.editAgent.section.heartbeat"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.heartbeatEnabled"), bool_badge(locale, detail.heartbeat_enabled)),
            kv_row(
                i18n::t(locale, "native.editAgent.row.heartbeatInterval"),
                plain_value(SharedString::from(detail.heartbeat_interval_seconds.to_string())),
            ),
        ]),
    );

    let notify_channel_val = detail.proactive_notify_channel.clone().map(SharedString::from).unwrap_or_else(unset);
    let notify_chat_id_val = detail.proactive_notify_chat_id.clone().map(SharedString::from).unwrap_or_else(unset);
    let quiet_hours_value: Div = match &detail.proactive_quiet_hours {
        Some(window) => {
            let mut col = div().flex().flex_col().gap_0p5().child(plain_value(SharedString::from(window.clone())));
            if let Some(note) = &detail.proactive_quiet_hours_note {
                col = col.child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(SharedString::from(note.clone())));
            }
            col
        }
        None => plain_value(i18n::t(locale, "native.editAgent.quietHoursNone")),
    };
    let proactive_section = section(
        i18n::t(locale, "native.editAgent.section.proactive"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.proactiveEnabled"), bool_badge(locale, detail.proactive_enabled)),
            kv_row(i18n::t(locale, "native.editAgent.row.notifyChannel"), plain_value(notify_channel_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.notifyChatId"), plain_value(notify_chat_id_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.quietHours"), quiet_hours_value),
        ]),
    );

    let evolution_section = section(
        i18n::t(locale, "native.editAgent.section.evolution"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.gvuEnabled"), bool_badge(locale, detail.gvu_enabled)),
            kv_row(
                i18n::t(locale, "native.editAgent.row.maxSilenceHours"),
                plain_value(SharedString::from(format_hours(detail.max_silence_hours))),
            ),
        ]),
    );

    let hour_value = detail.research_self_study_hour.map(|h| SharedString::from(h.to_string())).unwrap_or_else(unset);
    let research_section = section(
        i18n::t(locale, "native.editAgent.section.research"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.selfStudy"), bool_badge(locale, detail.research_self_study)),
            kv_row(i18n::t(locale, "native.editAgent.row.selfStudyHour"), plain_value(hour_value)),
        ]),
    );

    div()
        .flex()
        .flex_col()
        .gap_3p5()
        .child(heartbeat_section)
        .child(proactive_section)
        .child(evolution_section)
        .child(research_section)
}

// ── 進階 ───────────────────────────────────────────────────────────────

pub(super) fn advanced_tab(locale: Locale, detail: &EditAgentDetail) -> Div {
    let sticker_section = section(
        i18n::t(locale, "native.editAgent.section.sticker"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.stickerEnabled"), bool_badge(locale, detail.sticker_enabled)),
            kv_row(
                i18n::t(locale, "native.editAgent.row.stickerProbability"),
                plain_value(SharedString::from(format_percent(detail.sticker_probability))),
            ),
            kv_row(
                i18n::t(locale, "native.editAgent.row.stickerIntensity"),
                plain_value(SharedString::from(format_percent(detail.sticker_intensity_threshold))),
            ),
            kv_row(
                i18n::t(locale, "native.editAgent.row.stickerCooldown"),
                plain_value(SharedString::from(detail.sticker_cooldown_messages.to_string())),
            ),
            kv_row(
                i18n::t(locale, "native.editAgent.row.stickerExpressiveness"),
                plain_value(expressiveness_label(locale, &detail.sticker_expressiveness)),
            ),
        ]),
    );

    // 誠實偏差: web 表單「進階模型參數」(ptc/prompt/cultural_context 表格)
    // 純粹是設定檔欄位，`agents.inspect` 完全沒有回讀路徑 —— 本頁不臆造表
    // 格內容，只留一行誠實說明。
    let advanced_model_section = section(
        i18n::t(locale, "native.editAgent.section.advancedModelParams"),
        boxed_group(vec![div()
            .px_3p5()
            .py_3()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.editAgent.advancedModelParamsNote"))]),
    );

    div().flex().flex_col().gap_3p5().child(sticker_section).child(advanced_model_section)
}
