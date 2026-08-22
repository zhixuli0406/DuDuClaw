// S5b1-A (2026-08-21) — sibling of `device.rs`, holding the "備份與還原" and
// "危險區" sections of the "裝置" page (split out purely to keep `device.rs`
// under this crate's own <800-line file convention — see that file's own
// doc comment). Reads/writes the SAME `DeviceState` global `device.rs`
// defines; nothing here owns its own state struct.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, not guessed — line numbers as of this pass) ─────────────
//   `device.backup_create` (~L44057) → `{ "filename", "stdout", "stderr" }`
//     on success — the file lands under the shared attachments dir but this
//     page never offers a direct download link for it (see deviation #1
//     below); `device.backup_list` (~L44161) → `{ "files": [ {"name",
//     "size","mtime"} ] }` (epoch ms) is the one this page actually renders
//     rows from, over `<home>/backups/`, a DIFFERENT directory.
//   `device.backup_schedule_get`/`_set` (~L44078/44090) →
//     `{ "schedule_enabled", "interval_hours", "retention_count" }`.
//   `device.backup_delete` (~L44174, params `{ "name" }`) →
//     `{ "deleted": true }`.
//   `device.backup_restore` / the backup-upload HTTP endpoint are NOT
//     called anywhere in this file — see deviation #2.
//   `device.factory_reset` (~L44023) / `device.power` (~L44031, params
//     `{ "action": "restart"|"shutdown" }`) — both additionally gated by
//     `require_confirm!()` server-side (`params.confirm == true` or the
//     call is refused with `{"code":"confirm_required"}`); every call this
//     file makes to either method sends `"confirm": true` ONLY after the
//     user has gone through this page's own inline arm-then-confirm step
//     (see `danger_row` below) — never inferred, never sent speculatively.
//
// ── Deliberate degradations, documented rather than faked ────────────────
// 1. `device.backup_create`'s result is shown as a plain "已建立備份：
//    <filename>" line, not a download link — that archive is staged under
//    the shared task/channel ATTACHMENTS directory
//    (`GET /api/files/download?name=...`), a different download surface
//    than `device.backup_list`'s own `<home>/backups/` rows (which DO get a
//    real download action below, `backup_download_url`). Wiring the
//    attachments-download path too was judged not worth a second URL
//    builder + a second undocumented endpoint assumption for one button;
//    the file is still real and still on disk, just not one click away
//    from this page — an honest scope cut, not a silent gap.
// 2. "從舊機匯入" (`device.backup_restore`) needs a multipart file upload
//    (`POST /api/device/backup-upload`) BEFORE the RPC call — this crate's
//    RPC layer (`rpc.rs`/`ws_status.rs`) is JSON-RPC-over-WebSocket only,
//    and there is no file-picker dialog wired anywhere in this crate yet.
//    Rather than half-build a picker with no upload path behind it, this
//    section renders inert with a "請改用網頁版儀表板操作" note — same "an
//    inert control is honest; a silently-no-op one is a worse lie"
//    precedent `screens/agents_detail.rs`'s read-only capability rows and
//    `screens/accounts.rs`'s "新增帳號" row both already establish.
// 3. Interval/retention are bounded +/- steppers, not free-text keyboard
//    entry — the canvas itself shows this row as static copy ("每 24 小時
//    自動備份，保留最近 7 份"), not an input field; wiring a real
//    `TextField` `Entity` here would also need syncing its content on every
//    successful `backup_schedule_get`/`_set`, a second state-sync path this
//    page doesn't otherwise need. Steppers deliver the same real
//    functionality (an editable, savable value, matching `web/src/pages/
//    DevicePage.tsx`'s own editable interval/retention — functional
//    reference only) without inventing a keyboard-entry surface the canvas
//    never actually draws.
// 4. No modal confirm dialog for the danger zone or backup deletion —
//    `mds_gpui::dialog_overlay` exists but is unused crate-wide (its own
//    doc comment: "not yet exercised... a caller stacking a dialog over its
//    own internally-scrolled content should reach for `gpui::deferred`").
//    This page's own scroll container makes it exactly that case, so this
//    round reuses the simpler "arm on first click, confirm/cancel replaces
//    the trigger inline" pattern `goals_inspector.rs`'s own retry-note
//    toggle already establishes for this crate, rather than being the
//    first caller to work through the deferred-overlay-inside-scroll
//    question for two low-traffic settings rows.

use gpui::{div, prelude::*, px, ClickEvent, Context, Div, ElementId, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, skeleton, ButtonVariant};
use crate::screens::device::{
    self, boxed_group, error_line, section_label, BackupFile, DangerAction, DeviceState, Loadable,
};
use crate::theme;
use crate::RootView;

// ── Response parsing (moved from `device.rs` to keep that file under this
// crate's own <800-line convention — `device.rs::fetch_secondary` imports
// both back via `use crate::screens::device_backup::{...}`) ─────────────

pub(super) fn parse_backup_schedule(v: &Value) -> device::BackupSchedule {
    device::BackupSchedule {
        enabled: v.get("schedule_enabled").and_then(Value::as_bool).unwrap_or(false),
        interval_hours: v.get("interval_hours").and_then(Value::as_u64).unwrap_or(24),
        retention_count: v.get("retention_count").and_then(Value::as_u64).unwrap_or(7),
    }
}

pub(super) fn parse_backup_files(v: &Value) -> Vec<BackupFile> {
    v.get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| BackupFile {
            name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            size: f.get("size").and_then(Value::as_u64).unwrap_or(0),
            mtime_ms: f.get("mtime").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

// ── Small local primitives ────────────────────────────────────────────

fn toggle_switch(
    id: impl Into<ElementId>,
    checked: bool,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .w(px(36.))
        .h(px(21.))
        .flex_shrink_0()
        .rounded_full()
        .cursor_pointer()
        .bg(theme::alpha(if checked { theme::BRAND } else { theme::MUTED }, 1.0))
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(if checked { px(17.) } else { px(2.) })
                .size(px(17.))
                .rounded_full()
                .bg(theme::alpha(0xffffff, 1.0)),
        )
        .on_click(on_click)
}

fn row_frame(is_last: bool) -> Div {
    let row = div().flex().flex_col().gap_2p5().px_4().py_3();
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn kv_head(title: SharedString, desc: SharedString, trailing: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
                .child(div().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(desc)),
        )
        .child(trailing)
}

// ── Backup create ──────────────────────────────────────────────────────

fn dispatch_backup_create(cx: &mut Context<RootView>, state: &RootView) {
    let tx = state.session_tx.clone();
    {
        let g = cx.default_global::<DeviceState>();
        g.backup_creating = true;
        g.backup_create_result = None;
    }
    let tx2 = tx.clone();
    device::spawn_call(cx, tx, "device.backup_create", json!({}), move |cx, result| {
        let g = cx.default_global::<DeviceState>();
        g.backup_creating = false;
        match result {
            Ok(v) => {
                let filename = v.get("filename").and_then(Value::as_str).unwrap_or("").to_string();
                g.backup_create_result = Some(Ok(filename));
                g.backups = Loadable::Loading;
                device::spawn_call(cx, tx2, "device.backup_list", json!({}), |cx, result| {
                    cx.default_global::<DeviceState>().backups = result.map(|v| parse_backup_files(&v)).into();
                });
            }
            Err(e) => g.backup_create_result = Some(Err(e)),
        }
    });
}

fn backup_create_row(locale: Locale, _state: &RootView, cx: &mut Context<RootView>) -> Div {
    let g = cx.default_global::<DeviceState>();
    let creating = g.backup_creating;
    let result = g.backup_create_result.clone();

    let btn_label = if creating { i18n::t(locale, "device.backup.creating") } else { i18n::t(locale, "device.backup.create") };
    let cta = button("device-backup-create", btn_label, ButtonVariant::Primary, creating, None, cx.listener(|this, _ev, _window, cx| {
        dispatch_backup_create(cx, this);
    }));

    let mut col = kv_head(i18n::t(locale, "device.backup.create"), SharedString::default(), cta);
    if let Some(result) = result {
        let line = match result {
            Ok(filename) => (theme::SUCCESS, i18n::t1(locale, "device.backup.createdPrefix", "filename", &filename)),
            Err(msg) => (theme::DESTRUCTIVE, i18n::t1(locale, "native.home.card.errorPrefix", "message", &msg)),
        };
        col = div().flex().flex_col().gap_2().child(col).child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(line.0, 1.0)).child(line.1));
    }
    col
}

// ── Backup schedule ────────────────────────────────────────────────────

/// Which draft field a stepper row edits — replaces an earlier draft's
/// string-key comparison (`label_key == "interval"`) that could never match
/// (the actual keys are the full i18n paths, `"device.backup.schedule.
/// interval"`), caught before this ever shipped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScheduleField {
    Interval,
    Retention,
}

impl ScheduleField {
    fn label_key(self) -> &'static str {
        match self {
            Self::Interval => "device.backup.schedule.interval",
            Self::Retention => "device.backup.schedule.retention",
        }
    }
    fn id_tag(self) -> &'static str {
        match self {
            Self::Interval => "interval",
            Self::Retention => "retention",
        }
    }
    fn get(self, g: &DeviceState) -> u64 {
        match self {
            Self::Interval => g.draft_interval,
            Self::Retention => g.draft_retention,
        }
    }
    fn set(self, g: &mut DeviceState, v: u64) {
        match self {
            Self::Interval => g.draft_interval = v,
            Self::Retention => g.draft_retention = v,
        }
    }
}

fn schedule_number_row(locale: Locale, field: ScheduleField, min: u64, max: u64, cx: &mut Context<RootView>) -> Div {
    let value = field.get(cx.default_global::<DeviceState>());
    let dec = div()
        .id(format!("device-sched-dec-{}", field.id_tag()))
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::alpha(theme::MUTED, if value > min { 1.0 } else { 0.4 }))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .cursor_pointer()
        .child("−")
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            let g = cx.default_global::<DeviceState>();
            let v = field.get(g);
            if v > min {
                field.set(g, v - 1);
            }
            cx.notify();
        }));
    let inc = div()
        .id(format!("device-sched-inc-{}", field.id_tag()))
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::alpha(theme::MUTED, if value < max { 1.0 } else { 0.4 }))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .cursor_pointer()
        .child("+")
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            let g = cx.default_global::<DeviceState>();
            let v = field.get(g);
            if v < max {
                field.set(g, v + 1);
            }
            cx.notify();
        }));

    div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, field.label_key())))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(dec)
                .child(div().w(px(28.)).text_size(px(theme::TEXT_SM)).text_center().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value.to_string()))
                .child(inc),
        )
}

fn dispatch_schedule_save(cx: &mut Context<RootView>, state: &RootView) {
    let tx = state.session_tx.clone();
    let (enabled, interval, retention) = {
        let g = cx.default_global::<DeviceState>();
        g.schedule_saving = true;
        g.schedule_save_result = None;
        (g.draft_enabled, g.draft_interval, g.draft_retention)
    };
    let params = json!({ "schedule_enabled": enabled, "interval_hours": interval, "retention_count": retention });
    device::spawn_call(cx, tx, "device.backup_schedule_set", params, |cx, result| {
        let g = cx.default_global::<DeviceState>();
        g.schedule_saving = false;
        match result {
            Ok(v) => {
                let sched = parse_backup_schedule(&v);
                g.draft_enabled = sched.enabled;
                g.draft_interval = sched.interval_hours;
                g.draft_retention = sched.retention_count;
                g.schedule = Loadable::Ready(sched);
                g.schedule_save_result = Some(Ok(()));
            }
            Err(e) => g.schedule_save_result = Some(Err(e)),
        }
    });
}

fn backup_schedule_row(locale: Locale, _state: &RootView, cx: &mut Context<RootView>) -> Div {
    let g = cx.default_global::<DeviceState>();
    let draft_enabled = g.draft_enabled;
    let saving = g.schedule_saving;
    let save_result = g.schedule_save_result.clone();
    let schedule_loading = matches!(g.schedule, Loadable::Loading);

    let toggle = toggle_switch("device-sched-toggle", draft_enabled, cx.listener(|_this, _ev, _window, cx| {
        let g = cx.default_global::<DeviceState>();
        g.draft_enabled = !g.draft_enabled;
        cx.notify();
    }));

    let mut col = div()
        .flex()
        .flex_col()
        .gap_2p5()
        .child(kv_head(i18n::t(locale, "device.backup.schedule.title"), i18n::t(locale, "device.backup.schedule.desc"), toggle));

    if draft_enabled {
        col = col
            .child(schedule_number_row(locale, ScheduleField::Interval, 1, 8760, cx))
            .child(schedule_number_row(locale, ScheduleField::Retention, 1, 1000, cx));
        let save_label = if saving { i18n::t(locale, "device.backup.schedule.saving") } else { i18n::t(locale, "device.backup.schedule.save") };
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(button("device-sched-save", save_label, ButtonVariant::Secondary, saving || schedule_loading, None, cx.listener(|this, _ev, _window, cx| {
                    dispatch_schedule_save(cx, this);
                })))
                .children(save_result.map(|r| match r {
                    Ok(()) => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::SUCCESS, 1.0)).child(i18n::t(locale, "device.backup.schedule.saved")),
                    Err(msg) => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(i18n::t1(locale, "native.home.card.errorPrefix", "message", &msg)),
                })),
        );
    }
    col
}

// ── Backup list ─────────────────────────────────────────────────────────

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn short_date(mtime_ms: i64) -> String {
    let secs = mtime_ms / 1000;
    let days_since_epoch = secs / 86400;
    // Deliberately not a calendar-accurate date (no `chrono` `DateTime`
    // formatting is worth pulling in here for one relative-ish label) — a
    // plain "days since epoch" tag still lets rows sort/compare visually.
    // `device.backup.list.col.time` renders whatever this returns verbatim.
    format!("{days_since_epoch}d")
}

fn dispatch_backup_delete(cx: &mut Context<RootView>, state: &RootView, name: String) {
    let tx = state.session_tx.clone();
    {
        let g = cx.default_global::<DeviceState>();
        g.deleting_name = Some(name.clone());
        g.delete_result = None;
    }
    let params = json!({ "name": name });
    device::spawn_call(cx, tx.clone(), "device.backup_delete", params, move |cx, result| {
        let g = cx.default_global::<DeviceState>();
        g.deleting_name = None;
        match result {
            Ok(_) => {
                g.delete_result = Some(Ok(()));
                g.delete_armed = None;
                g.backups = Loadable::Loading;
                device::spawn_call(cx, tx, "device.backup_list", json!({}), |cx, result| {
                    cx.default_global::<DeviceState>().backups = result.map(|v| parse_backup_files(&v)).into();
                });
            }
            Err(e) => g.delete_result = Some(Err(e)),
        }
    });
}

fn backup_file_row(locale: Locale, file: &BackupFile, armed: bool, deleting: bool, cx: &mut Context<RootView>) -> Div {
    let name_display = SharedString::from(file.name.clone());
    let meta = format!("{} · {}", format_bytes(file.size), short_date(file.mtime_ms));

    let trailing: Div = if armed {
        let name_for_confirm = file.name.clone();
        let name_for_cancel = file.name.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_size(px(11.)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(i18n::t1(locale, "device.backup.list.deleteConfirm", "name", &file.name)))
            .child(button("device-backup-delete-confirm", i18n::t(locale, "device.backup.list.deleteConfirmYes"), ButtonVariant::Destructive, deleting, None, cx.listener(move |this, _ev, _window, cx| {
                dispatch_backup_delete(cx, this, name_for_confirm.clone());
            })))
            .child(button("device-backup-delete-cancel", i18n::t(locale, "device.backup.list.deleteCancel"), ButtonVariant::Ghost, false, None, cx.listener(move |_this, _ev, _window, cx| {
                let _ = &name_for_cancel;
                cx.default_global::<DeviceState>().delete_armed = None;
                cx.notify();
            })))
    } else {
        let name_for_arm = file.name.clone();
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id(format!("device-backup-download-{name_for_arm}"))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::BRAND, 1.0))
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(i18n::t(locale, "device.backup.list.download"))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        let url = backup_download_url(this, &name_for_arm);
                        cx.open_url(&url);
                    })),
            )
            .child({
                let name_for_delete = file.name.clone();
                div()
                    .id(format!("device-backup-delete-arm-{}", file.name))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(i18n::t(locale, "device.backup.list.delete"))
                    .on_click(cx.listener(move |_this, _ev, _window, cx| {
                        cx.default_global::<DeviceState>().delete_armed = Some(name_for_delete.clone());
                        cx.notify();
                    }))
            })
    };

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(div().text_size(px(11.5)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(name_display))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(meta)),
        )
        .child(trailing)
}

/// `GET /api/files/download` — the download endpoint every other file-
/// bearing page in this crate is documented as NOT yet reaching (`main.rs`'s
/// field list shows `jwt` captured but unused elsewhere); this is the first
/// caller. Query values are plain ASCII (server-generated `backup-<date>.
/// tar.gz` names, `duduclaw_gateway::files_api`'s own naming convention) so
/// no percent-encoding helper is pulled in for this one call site.
fn backup_download_url(state: &RootView, name: &str) -> String {
    let token = state.jwt.as_deref().unwrap_or("");
    format!("{}/api/device/backups/download?name={name}&token={token}", crate::api::gateway_base_url())
}

fn backup_list_section(locale: Locale, _state: &RootView, cx: &mut Context<RootView>) -> Div {
    let g = cx.default_global::<DeviceState>();
    let backups = g.backups.clone();
    let armed = g.delete_armed.clone();
    let deleting = g.deleting_name.clone();

    let title = div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "device.backup.list.title"));

    let body: Div = match backups {
        Loadable::Loading => div().mt_2().child(skeleton(px(300.), px(16.))),
        Loadable::Failed(msg) => div().mt_2().child(error_line(locale, &msg)),
        Loadable::Ready(files) if files.is_empty() => div()
            .mt_2()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "device.backup.list.empty")),
        Loadable::Ready(files) => {
            let mut wrap = div().mt_2().flex().flex_col().gap_2p5();
            for file in &files {
                let is_armed = armed.as_deref() == Some(file.name.as_str());
                let is_deleting = deleting.as_deref() == Some(file.name.as_str());
                wrap = wrap.child(backup_file_row(locale, file, is_armed, is_deleting, cx));
            }
            wrap
        }
    };

    div().flex().flex_col().child(title).child(body)
}

// ── 從舊機匯入（inert — see header comment #2）────────────────────────

fn restore_row(locale: Locale) -> Div {
    kv_head(
        i18n::t(locale, "device.restore.title"),
        i18n::t(locale, "device.restore.desc"),
        div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "device.restore.comingSoon")),
    )
}

// ── Top-level: 備份與還原 ──────────────────────────────────────────────

pub(super) fn render_backup_section(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let rows = vec![
        row_frame(false).child(backup_create_row(locale, state, cx)),
        row_frame(false).child(backup_schedule_row(locale, state, cx)),
        row_frame(false).child(backup_list_section(locale, state, cx)),
        row_frame(true).child(restore_row(locale)),
    ];
    div().flex().flex_col().gap_1p5().child(section_label(locale, "device.section.backup")).child(boxed_group(rows))
}

// ── Top-level: 危險區 ───────────────────────────────────────────────────

fn danger_action_keys(action: DangerAction) -> (&'static str, &'static str, &'static str, &'static str) {
    match action {
        DangerAction::FactoryReset => ("device.danger.factoryReset.title", "device.danger.factoryReset.desc", "device.danger.factoryReset.cta", "device.danger.factoryReset.confirm"),
        DangerAction::Restart => ("device.danger.restart.title", "device.danger.restart.desc", "device.danger.restart.cta", "device.danger.restart.confirm"),
        DangerAction::Shutdown => ("device.danger.shutdown.title", "device.danger.shutdown.desc", "device.danger.shutdown.cta", "device.danger.shutdown.confirm"),
    }
}

fn dispatch_danger_action(cx: &mut Context<RootView>, state: &RootView, action: DangerAction) {
    let tx = state.session_tx.clone();
    {
        let g = cx.default_global::<DeviceState>();
        g.danger_busy = true;
        g.danger_result = None;
    }
    let (method, params): (&'static str, Value) = match action {
        DangerAction::FactoryReset => ("device.factory_reset", json!({ "confirm": true })),
        DangerAction::Restart => ("device.power", json!({ "action": "restart", "confirm": true })),
        DangerAction::Shutdown => ("device.power", json!({ "action": "shutdown", "confirm": true })),
    };
    device::spawn_call(cx, tx, method, params, |cx, result| {
        let g = cx.default_global::<DeviceState>();
        g.danger_busy = false;
        match result {
            Ok(v) => {
                let success = v.get("success").and_then(Value::as_bool).unwrap_or(false);
                if success {
                    // Locale-independent: the "已送出" text is rendered from
                    // the CURRENT locale at render time by
                    // `render_danger_section`, never baked in here — same
                    // rule `backup_create_result`/`schedule_save_result`
                    // already follow for exactly this reason.
                    g.danger_result = Some(Ok(()));
                    g.danger_armed = None;
                } else {
                    let stderr = v.get("stderr").and_then(Value::as_str).unwrap_or("").to_string();
                    g.danger_result = Some(Err(stderr));
                }
            }
            Err(e) => g.danger_result = Some(Err(e)),
        }
    });
}

fn danger_row(locale: Locale, action: DangerAction, armed: Option<DangerAction>, busy: bool, is_last: bool, cx: &mut Context<RootView>) -> Div {
    let (title_key, desc_key, cta_key, confirm_key) = danger_action_keys(action);
    let is_armed = armed == Some(action);

    let trailing: Div = if is_armed {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(button("device-danger-confirm", i18n::t(locale, "device.danger.confirmYes"), ButtonVariant::Destructive, busy, None, cx.listener(move |this, _ev, _window, cx| {
                dispatch_danger_action(cx, this, action);
            })))
            .child(button("device-danger-cancel", i18n::t(locale, "device.danger.confirmCancel"), ButtonVariant::Ghost, false, None, cx.listener(|_this, _ev, _window, cx| {
                cx.default_global::<DeviceState>().danger_armed = None;
                cx.notify();
            })))
    } else {
        div().child(button(format!("device-danger-arm-{title_key}"), i18n::t(locale, cta_key), ButtonVariant::Destructive, false, None, cx.listener(move |_this, _ev, _window, cx| {
            let g = cx.default_global::<DeviceState>();
            g.danger_armed = Some(action);
            g.danger_result = None;
            cx.notify();
        })))
    };

    let mut col = kv_head(i18n::t(locale, title_key), i18n::t(locale, desc_key), trailing);
    if is_armed {
        col = div().flex().flex_col().gap_1p5().child(col).child(div().text_size(px(11.)).text_color(theme::alpha(theme::WARNING, 1.0)).child(i18n::t(locale, confirm_key)));
    }
    let row = div().flex().flex_col().px_4().py_3().child(col);
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

pub(super) fn render_danger_section(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let g = cx.default_global::<DeviceState>();
    let armed = g.danger_armed;
    let busy = g.danger_busy;
    let result = g.danger_result.clone();

    let rows = vec![
        danger_row(locale, DangerAction::FactoryReset, armed, busy, false, cx),
        danger_row(locale, DangerAction::Restart, armed, busy, false, cx),
        danger_row(locale, DangerAction::Shutdown, armed, busy, true, cx),
    ];

    let mut section = div().flex().flex_col().gap_1p5().child(section_label(locale, "device.section.danger")).child(
        div()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(theme::RADIUS_XL))
            .overflow_hidden()
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::alpha(theme::DESTRUCTIVE, 0.22))
            .children(rows),
    );
    match result {
        Some(Ok(())) => {
            section = section.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::SUCCESS, 1.0)).child(i18n::t(locale, "device.danger.done")));
        }
        Some(Err(msg)) => section = section.child(error_line(locale, &msg)),
        None => {}
    }
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backup_schedule_reads_the_real_payload_shape() {
        let v = json!({ "schedule_enabled": true, "interval_hours": 24, "retention_count": 7 });
        let s = parse_backup_schedule(&v);
        assert!(s.enabled);
        assert_eq!(s.interval_hours, 24);
        assert_eq!(s.retention_count, 7);
    }

    #[test]
    fn parse_backup_schedule_missing_fields_falls_back_to_the_server_defaults() {
        let s = parse_backup_schedule(&json!({}));
        assert!(!s.enabled);
        assert_eq!(s.interval_hours, 24);
        assert_eq!(s.retention_count, 7);
    }

    #[test]
    fn parse_backup_files_reads_the_files_api_shape() {
        let v = json!({ "files": [
            {"name": "backup-2026-08-20.tar.gz", "size": 432013312_u64, "mtime": 1755600000000_i64},
        ]});
        let files = parse_backup_files(&v);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "backup-2026-08-20.tar.gz");
        assert_eq!(files[0].size, 432013312);
        assert_eq!(files[0].mtime_ms, 1755600000000);
    }

    #[test]
    fn parse_backup_files_missing_array_is_empty_not_panicking() {
        assert!(parse_backup_files(&json!({})).is_empty());
    }
}
