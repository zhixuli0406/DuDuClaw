// WP-S5b1-C — Screen "身分解析" (`nav.rs` id `identity`, wired by the
// parallel A/B "整合" workstream — this pass adds the screen + its
// `screens/mod.rs` line only). An "整合" drill-down leaf: no own sidebar
// row, breadcrumb only.
//
// Visual authority: `commercial/design/duduclaw-s5-settings-pages/Identity.
// dc.html` — breadcrumb → header → "資料來源" provider pills → "Wiki 快取
//設定" boxed-list → "試著解析一個聯絡方式" test box → a 儲存 button.
// Functional reference: `web/src/pages/IdentityPage.tsx` (456 lines,
// full editable provider/Notion config form this drill-down does NOT
// reproduce — read-only config display + a genuinely live test-resolve).
//
// ── RPC shapes (verified against `crates/duduclaw-gateway/src/handlers.rs`,
// not guessed) ────────────────────────────────────────────────────────────
//   `identity.config_get` (dispatch ~L5515, handler ~L9892,
//     `require_admin!()`) → `{ provider: "wiki_cache"|"notion"|"chained",
//     notion: {...}, wiki_cache: { people_dir } }`.
//   `identity.resolve` (dispatch ~L5523, handler ~L10097,
//     `require_admin!()`) — request `{ identifier, channel? }` (`channel`
//     wire names verified against `ChannelKind::parse_wire`: email/discord/
//     line/telegram/slack/whatsapp/feishu/webchat, mirrored from `web/src/
//     pages/IdentityPage.tsx`'s own `RESOLVE_CHANNELS`). Response is a
//     SUCCESS frame either way (miss is not an error, confirmed by gateway
//     test `resolve_miss_is_found_false_not_error`): `{ found: true,
//     provider, channel, is_project_member, person: { person_id,
//     display_name, roles, project_ids, emails, channel_handles, source,
//     fetched_at } }` on a hit, `{ found: false, provider, channel }`
//     (no `person` key at all) on a miss. A genuinely missing `identifier`
//     param IS an error frame — this page never sends an empty one (the
//     解析 button is disabled while the field is blank), so that path is
//     defensive-only, not exercised by normal use.
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. No "已收錄名單：18 位" row. `identity.config_get`'s `wiki_cache` object
//    carries only `people_dir` — no RPC counts registered people-directory
//    profiles. Fabricating a count (the canvas's "18 位" is mockup flavor
//    text) would be exactly the kind of invented data `about.rs`'s own
//    "Honest stub: no Build 日期 row" precedent exists to avoid; this row
//    is dropped rather than shipped as a lie.
// 2. Provider pills (Wiki 快取／Notion／串接) are READ-ONLY — they highlight
//    whichever provider `identity.config_get` reports as active, but are
//    not clickable. Switching providers is a config WRITE
//    (`identity.config_set`), out of this drill-down's explicit scope
//    (task brief only asks for "提供者選擇" as a display element + the live
//    test-resolve box — no write-form scope was asked for). The design
//    canvas's 儲存 button is rendered the same disabled way every other
//    decision-type action across this page batch is (see `odoo.rs`'s
//    header comment) — present, not wired.
// 3. **The test-resolve box IS fully live** — the one genuinely wired
//    action across all four pages this batch ships, per this task's brief
//    ("測試解析可真按——唯讀查詢類"). Channel selection and the identifier
//    field are real inputs (a "live" resolve button whose channel/input
//    were fake pills would be a worse lie than a disabled one), calling the
//    real `identity.resolve` RPC and rendering its real three-way outcome
//    (found / not-found / error) — never a fabricated result.

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, skeleton, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::settings_common::{boxed_group, breadcrumb, kv_row, section_label};
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

/// Wire channel names — verified against `ChannelKind::parse_wire`, mirrors
/// `web/src/pages/IdentityPage.tsx`'s own `RESOLVE_CHANNELS`.
pub const RESOLVE_CHANNELS: &[&str] =
    &["email", "discord", "line", "telegram", "slack", "whatsapp", "feishu", "webchat"];

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct IdentityConfigInfo {
    /// Raw `"wiki_cache"|"notion"|"chained"`.
    pub provider: String,
    pub people_dir: String,
}

#[derive(Clone, Default)]
pub struct ResolvedPersonInfo {
    pub display_name: String,
    pub roles: Vec<String>,
    pub project_ids: Vec<String>,
    pub source: String,
}

#[derive(Clone)]
pub enum ResolveOutcome {
    Found { person: ResolvedPersonInfo, is_project_member: bool },
    NotFound,
}

pub struct IdentityState {
    requested: bool,
    pub config: Loadable<IdentityConfigInfo>,
    pub resolve_input: Entity<TextField>,
    pub resolve_channel: String,
    /// `None` = never tried yet (result box hidden entirely, not shown as
    /// an empty/error state — nothing has happened, so nothing is claimed).
    pub resolve_result: Option<Loadable<ResolveOutcome>>,
}

impl IdentityState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            requested: false,
            config: Loadable::Loading,
            resolve_input: TextField::new(cx, "louis@dudustudio.monster", false, ""),
            resolve_channel: "email".to_string(),
            resolve_result: None,
        }
    }
}

impl Global for IdentityState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<IdentityState>() {
        let state = IdentityState::new(cx);
        cx.set_global(state);
    }
}

// ── Response parsing ──────────────────────────────────────────────────

pub fn parse_identity_config(v: &Value) -> IdentityConfigInfo {
    IdentityConfigInfo {
        provider: v.get("provider").and_then(Value::as_str).unwrap_or("wiki_cache").to_string(),
        people_dir: v.get("wiki_cache").and_then(|w| w.get("people_dir")).and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

pub fn parse_resolve_response(v: &Value) -> ResolveOutcome {
    let found = v.get("found").and_then(Value::as_bool).unwrap_or(false);
    if !found {
        return ResolveOutcome::NotFound;
    }
    let is_project_member = v.get("is_project_member").and_then(Value::as_bool).unwrap_or(false);
    let person = v.get("person");
    let display_name = person.and_then(|p| p.get("display_name")).and_then(Value::as_str).unwrap_or("").to_string();
    let roles: Vec<String> = person
        .and_then(|p| p.get("roles"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let project_ids: Vec<String> = person
        .and_then(|p| p.get("project_ids"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let source = person.and_then(|p| p.get("source")).and_then(Value::as_str).unwrap_or("").to_string();
    ResolveOutcome::Found { person: ResolvedPersonInfo { display_name, roles, project_ids, source }, is_project_member }
}

/// `identity.resolve` params — `handle_identity_resolve` reads exactly
/// `identifier`/`channel` (default `"email"` when absent). Pure + unit
/// tested per this crate's "決策類 RPC 以單元測試驗 request 組裝" convention
/// (`console.rs::build_approval_decide_params` establishes the pattern) —
/// this one IS actually wired (see header comment §3), so the builder
/// doubles as the real call-site's param source, not just a documented stub.
pub fn build_resolve_params(identifier: &str, channel: &str) -> Value {
    json!({ "identifier": identifier, "channel": channel })
}

// ── Fetch orchestration ──────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<IdentityState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<IdentityState>().requested = true;
    let tx = state.session_tx.clone();

    spawn_call(cx, tx, "identity.config_get", json!({}), |cx, result| {
        cx.global_mut::<IdentityState>().config = result.map(|v| parse_identity_config(&v)).into();
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

/// Submit the test-resolve query — the one real write-path (a read-only RPC
/// call, no state mutation server-side) this page fires. Called from the
/// 解析 button's `on_click`.
fn submit_resolve(view: &mut RootView, cx: &mut Context<RootView>) {
    let (identifier, channel) = {
        let g = cx.global::<IdentityState>();
        (g.resolve_input.read(cx).content.trim().to_string(), g.resolve_channel.clone())
    };
    if identifier.is_empty() {
        return;
    }
    cx.global_mut::<IdentityState>().resolve_result = Some(Loadable::Loading);
    let tx = view.session_tx.clone();
    let params = build_resolve_params(&identifier, &channel);
    spawn_call(cx, tx, "identity.resolve", params, |cx, result| {
        cx.global_mut::<IdentityState>().resolve_result = Some(result.map(|v| parse_resolve_response(&v)).into());
    });
    cx.notify();
}

// ── Display helpers ────────────────────────────────────────────────────

fn provider_pill(locale: Locale, label_key: &str, active: bool) -> Div {
    div()
        .px_4()
        .py_2()
        .rounded(px(theme::RADIUS_LG))
        .text_size(px(theme::TEXT_SM))
        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .when(active, |el| {
            el.bg(theme::alpha(theme::INFO, 0.12)).text_color(theme::alpha(theme::INFO, 1.0)).border_1().border_color(theme::alpha(theme::INFO, 0.35))
        })
        .when(!active, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).border_1().border_color(theme::surface_border())
        })
        .child(i18n::t(locale, label_key))
}

fn channel_pill(locale: Locale, channel: &'static str, active: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let key = format!("native.identity.channel.{channel}");
    let label: SharedString = i18n::t(locale, &key);
    div()
        .id(SharedString::from(format!("identity-channel-{channel}")))
        .px_2p5()
        .py_1()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .when(active, |el| el.bg(theme::alpha(theme::PRIMARY, 1.0)).text_color(theme::alpha(theme::PRIMARY_FOREGROUND, 1.0)))
        .when(!active, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).border_1().border_color(theme::surface_border())
        })
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<IdentityState>().resolve_channel = channel.to_string();
            cx.notify();
        }))
}

fn resolve_result_box(locale: Locale, outcome: &Loadable<ResolveOutcome>) -> Div {
    match outcome {
        Loadable::Loading => div()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::MUTED, 0.5))
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.identity.resolving"))),
        Loadable::Failed(err) => div()
            .p_3()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::DESTRUCTIVE, 0.08))
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone()))),
        Loadable::Ready(ResolveOutcome::NotFound) => div()
            .p_3()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::MUTED, 0.5))
            .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.identity.result.notFound"))),
        Loadable::Ready(ResolveOutcome::Found { person, is_project_member }) => div()
            .flex()
            .items_center()
            .gap_2p5()
            .p_3()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::SUCCESS, 0.08))
            .border_1()
            .border_color(theme::alpha(theme::SUCCESS, 0.25))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if person.display_name.is_empty() { SharedString::from("—") } else { SharedString::from(person.display_name.clone()) }),
                    )
                    .children((!person.roles.is_empty()).then(|| {
                        div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "native.identity.result.roles", "roles", &person.roles.join(", ")))
                    }))
                    .children((!person.project_ids.is_empty()).then(|| {
                        div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "native.identity.result.projects", "projects", &person.project_ids.join(", ")))
                    }))
                    .children((!person.source.is_empty()).then(|| {
                        div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "native.identity.result.source", "source", &person.source))
                    })),
            )
            .child(badge(
                i18n::t(locale, if *is_project_member { "native.identity.result.member" } else { "native.identity.result.stranger" }),
                if *is_project_member { BadgeVariant::Success } else { BadgeVariant::Secondary },
            )),
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let config = cx.global::<IdentityState>().config.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.identity.title")),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.identity.subtitle")),
        );

    let active_provider = match &config {
        Loadable::Ready(c) => c.provider.clone(),
        _ => "wiki_cache".to_string(),
    };
    let provider_section = div()
        .flex()
        .flex_col()
        .child(section_label(i18n::t(locale, "native.identity.dataSource")))
        .child(
            div()
                .flex()
                .gap_2()
                .child(provider_pill(locale, "native.identity.provider.wikiCache", active_provider == "wiki_cache"))
                .child(provider_pill(locale, "native.identity.provider.notion", active_provider == "notion"))
                .child(provider_pill(locale, "native.identity.provider.chained", active_provider == "chained")),
        );

    let wiki_cache_section = {
        let body = match &config {
            Loadable::Loading => div().child(skeleton(px(780.), px(80.))),
            Loadable::Failed(err) => div().p_4().child(
                div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(SharedString::from(err.clone())),
            ),
            Loadable::Ready(cfg) => boxed_group(vec![kv_row(
                i18n::t(locale, "native.identity.peopleDir"),
                div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(if cfg.people_dir.is_empty() { SharedString::from("—") } else { SharedString::from(cfg.people_dir.clone()) }),
                true,
            )]),
        };
        div().flex().flex_col().child(section_label(i18n::t(locale, "native.identity.wikiCacheSettings"))).child(body)
    };

    let resolve_input_entity = cx.global::<IdentityState>().resolve_input.clone();
    let resolve_channel = cx.global::<IdentityState>().resolve_channel.clone();
    let resolve_result = cx.global::<IdentityState>().resolve_result.clone();
    let can_submit = !resolve_input_entity.read(cx).content.trim().is_empty()
        && !matches!(&resolve_result, Some(Loadable::Loading));

    let channel_row = div()
        .flex()
        .flex_wrap()
        .gap_1p5()
        .children(RESOLVE_CHANNELS.iter().copied().map(|c| channel_pill(locale, c, resolve_channel == c, cx)));

    let test_box = div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(resolve_input_entity))
                .child(button(
                    "identity-resolve-submit",
                    i18n::t(locale, "native.identity.testButton"),
                    ButtonVariant::Primary,
                    !can_submit,
                    None,
                    cx.listener(|this, _ev, _window, cx| submit_resolve(this, cx)),
                )),
        )
        .child(channel_row)
        .children(resolve_result.as_ref().map(|r| resolve_result_box(locale, r)));

    let test_section = div()
        .flex()
        .flex_col()
        .child(section_label(i18n::t(locale, "native.identity.testResolve")))
        .child(test_box);

    let save_row = div()
        .flex()
        .justify_end()
        .child(button("identity-save", i18n::t(locale, "native.identity.save"), ButtonVariant::Primary, true, None, |_ev, _window, _cx| {}));

    div()
        .id("identity-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(780.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .child(breadcrumb("identity-breadcrumb", locale, i18n::t(locale, "native.identity.breadcrumb"), cx))
                .child(header)
                .child(provider_section)
                .child(wiki_cache_section)
                .child(test_section)
                .child(save_row),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_identity_config_reads_provider_and_people_dir() {
        let v = json!({ "provider": "notion", "notion": {}, "wiki_cache": { "people_dir": "shared/wiki/identity/people/" } });
        let cfg = parse_identity_config(&v);
        assert_eq!(cfg.provider, "notion");
        assert_eq!(cfg.people_dir, "shared/wiki/identity/people/");
    }

    #[test]
    fn parse_identity_config_missing_fields_default_to_wiki_cache() {
        let cfg = parse_identity_config(&json!({}));
        assert_eq!(cfg.provider, "wiki_cache");
        assert_eq!(cfg.people_dir, "");
    }

    #[test]
    fn parse_resolve_response_found_reads_every_field() {
        let v = json!({
            "found": true, "provider": "wiki-cache", "channel": "email", "is_project_member": true,
            "person": { "person_id": "p1", "display_name": "Louis Li", "roles": ["負責人"],
                        "project_ids": ["duduclaw", "distributor-tw"], "emails": [], "channel_handles": {},
                        "source": "wiki-cache", "fetched_at": "2026-08-21T00:00:00Z" },
        });
        match parse_resolve_response(&v) {
            ResolveOutcome::Found { person, is_project_member } => {
                assert!(is_project_member);
                assert_eq!(person.display_name, "Louis Li");
                assert_eq!(person.roles, vec!["負責人".to_string()]);
                assert_eq!(person.project_ids.len(), 2);
                assert_eq!(person.source, "wiki-cache");
            }
            ResolveOutcome::NotFound => panic!("expected Found"),
        }
    }

    #[test]
    fn parse_resolve_response_miss_is_not_found_with_no_person_key() {
        let v = json!({ "found": false, "provider": "wiki-cache", "channel": "email" });
        assert!(matches!(parse_resolve_response(&v), ResolveOutcome::NotFound));
    }

    #[test]
    fn parse_resolve_response_missing_found_defaults_to_not_found() {
        assert!(matches!(parse_resolve_response(&json!({})), ResolveOutcome::NotFound));
    }

    #[test]
    fn build_resolve_params_shape() {
        assert_eq!(
            build_resolve_params("ruby@example.com", "email"),
            json!({ "identifier": "ruby@example.com", "channel": "email" })
        );
    }

    #[test]
    fn resolve_channels_matches_channel_kind_wire_names() {
        // Mirrors `web/src/pages/IdentityPage.tsx`'s `RESOLVE_CHANNELS` —
        // a drift here would silently send a channel string
        // `ChannelKind::parse_wire` degrades to `Other(...)` on.
        let expected: &[&str] = &["email", "discord", "line", "telegram", "slack", "whatsapp", "feishu", "webchat"];
        assert_eq!(RESOLVE_CHANNELS, expected);
    }
}
