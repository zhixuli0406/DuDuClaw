// Y20-P3 (2026-08-29) — real block-device enumeration + single-select pick,
// replacing the P2 "開發中" placeholder. Mirrors `oobe::steps::network`'s
// scan shape end to end: `DiskScanState::NotScanned` (an explicit "掃描磁碟"
// button — I/O is always click-triggered in this crate, never a render-time
// side effect, same discipline that file's own header comment documents) ->
// `Scanning` -> `Loaded`/`Failed`; `Loaded` with zero entries renders its
// own honest empty state.
//
// ── Y20-P5 (2026-09-04): closes the "picks the live medium itself" bug ────
// QEMU walkthrough evidence: screens/05-installer-disks.png shows
// `/dev/vda 32G` (the real target) AND `/dev/vdb 1.8G` (the virtio CD-ROM
// carrying THIS SAME session's install ISO) both offered as pickable rows.
// `/dev/vdb` happened to dodge the P3-era `sr*`/`loop*`/`ram*`/`fd*`
// name-prefix filter because a virtio-attached ISO9660 medium enumerates as
// a plain virtio-blk `disk`, not the `rom` type an IDE/SATA optical drive
// would report — `is_excluded_name` alone was never going to catch every
// live-medium topology (a real USB-stick live medium is `TYPE=disk` too,
// with no distinguishing name at all). Two independent fixes landed:
//   1. `RO` (lsblk's own read-only flag) became a real exclusion, not just
//      TYPE/name.
//   2. `find_live_medium_device` re-derives `duduclaw-os-install.sh`'s own
//      §1 "which disk carries the payload" search (`/media/realroot` ->
//      `/run/media/*` -> `/media/*` mount search, then `findmnt -no SOURCE`
//      + `lsblk -no PKNAME` to resolve a partition back to its parent disk)
//      and threads its result into the pure filter below as an explicit
//      exclusion.
//
// ── Y20-P6 (2026-09-05): two review findings on the Y20-P5 fix above ──────
// HIGH — `find_live_medium_device` failed OPEN *silently*: any of "not
// mounted yet", "payload filename differs", or "`findmnt`/`lsblk` failed to
// run" collapsed to the same `None`, which made `filter_install_targets`
// exclude nothing extra AND rendered no note — the exact `/dev/vdb` bug
// could come back with zero visible signal that detection had quietly given
// up. Two independent layers fix this:
//   (a) `filter_install_targets` gains a SECOND exclusion that does not
//       depend on the payload filename at all: any disk that currently has
//       a mounted filesystem — on itself, or on ANY of its partitions — is
//       never offered as a target (a fresh target disk has nothing mounted;
//       the live medium always does, at minimum its own ISO9660/rootfs
//       mount). This needs lsblk's own child/partition tree plus per-device
//       `MOUNTPOINTS`, so the scan moves from `lsblk -dno ...` (top-level
//       disks only, plain text) to `lsblk -J -o NAME,TYPE,SIZE,RO,MODEL,
//       MOUNTPOINTS` (the full device tree, JSON so nested children parse
//       without a hand-rolled indentation-aware text parser) — see
//       `parse_lsblk_json`/`node_has_any_mountpoint` below. `parse_lsblk_
//       rows` (the P3/P5-era flat-text parser) is gone; `LsblkRow` is now
//       built FROM the JSON tree instead of parsed directly from lsblk's
//       stdout.
//   (b) `find_live_medium_device` now returns a tri-state,
//       `LiveMediumDetection` (`Found`/`NotFound`/`Failed`), instead of
//       `Option<String>` — `NotFound` (the search ran cleanly, nothing
//       matched) stays silent (layer (a) above still protects a genuinely
//       mounted medium regardless of naming), but `Failed` (the image WAS
//       located — proof a live medium is present — but resolving it to a
//       device did not succeed) now renders its own visible warning via
//       `state::LiveMediumNote::DetectionFailed` — see that type's own doc
//       comment in `state.rs`.
// LOW — the exclusion note and (in `steps::progress`) the "preparing" label
// were hardcoded bilingual "zh · en" literals; both now route through
// `crate::i18n` (`Key::LiveInstallDiskExcludedMedium`/
// `Key::LiveInstallDiskMediumUnknown`) like the sibling `steps::network`
// (`LiveWifi*`) step already does — see `medium_note_text` below.
//
// `duduclaw-os-install.sh`'s own CANDIDATES check remains the fail-closed
// backstop underneath every layer of this file's own filtering (unchanged
// across every round above): even if this file's own live-medium detection
// AND mount check both somehow miss a topology, the script still rejects
// `DUDUCLAW_INSTALL_TARGET=<src>` with "不在可安裝清單內" before anything is
// ever written.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};
use serde::Deserialize;

use duduclaw_native_gui::theme;

use crate::i18n::{t, t1, Key, Locale};
use crate::oobe::widgets;
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::{DiskInfo, DiskScanState, LiveInstallFlow, LiveMediumNote};

pub(super) fn render(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title("選擇安裝目標磁碟 · Select install disk", palette))
        .child(widgets::subtitle("該磁碟上的所有資料稍後將被清除 · All data on the chosen disk will be erased", palette))
        .child(widgets::card(scan_body(flow, cx), palette))
}

fn scan_body(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    match flow.disk_scan() {
        DiskScanState::NotScanned => not_scanned_panel(flow, cx),
        DiskScanState::Scanning => scanning_panel(flow),
        DiskScanState::Failed(message) => failed_panel(flow, message, cx),
        DiskScanState::Loaded(disks) if disks.is_empty() => empty_panel(flow, cx),
        DiskScanState::Loaded(disks) => loaded_panel(disks, flow, cx),
    }
}

fn not_scanned_panel(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child("尚未列出磁碟 · No disks listed yet"),
        )
        .child(widgets::step_button(
            "live-install-disk-scan",
            "掃描磁碟 · Scan disks",
            widgets::StepButtonVariant::Primary,
            false,
            palette,
            click,
        ))
}

fn scanning_panel(flow: &LiveInstallFlow) -> Div {
    let palette = flow.palette();
    div()
        .flex()
        .items_center()
        .justify_center()
        .py(px(16.))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child("掃描中… · Scanning…"))
}

fn failed_panel(flow: &LiveInstallFlow, message: &str, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(
            // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): same
            // undefined-width flex_col-child overflow as `confirm.rs`'s
            // `warning_banner` — `message` is real `lsblk` stderr, unbounded
            // length, so this div needs an explicit width to wrap instead of
            // running past the card. Trades the parent's `.items_center()`
            // centering for this one child (acceptable: an overflowing error
            // message is worse than a left-aligned one).
            div()
                .w_full()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.destructive, 1.0))
                .child(format!("掃描失敗 · Scan failed：{message}")),
        )
        .child(widgets::step_button("live-install-disk-rescan", "重試 · Retry", widgets::StepButtonVariant::Secondary, false, palette, click))
}

fn empty_panel(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();
    let locale = flow.locale();
    let click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));
    let mut panel = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .py(px(8.))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child("找不到可安裝的磁碟 · No installable disk found"),
        );
    if let Some(note) = medium_note_div(flow.disk_medium_note(), locale, palette) {
        panel = panel.child(note);
    }
    panel.child(widgets::step_button("live-install-disk-rescan", "重新整理 · Rescan", widgets::StepButtonVariant::Secondary, false, palette, click))
}

fn loaded_panel(disks: &[DiskInfo], flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();
    let locale = flow.locale();
    let selected = flow.selected_disk().map(|d| d.name.clone());

    let rescan_click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));

    let mut rows = div().flex().flex_col().gap(px(6.));
    for (index, disk) in disks.iter().enumerate() {
        rows = rows.child(disk_row(disk, index, selected.as_deref() == Some(disk.name.as_str()), palette, cx));
    }

    let mut container = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div().flex().justify_end().child(widgets::step_button(
                "live-install-disk-rescan",
                "重新整理 · Rescan",
                widgets::StepButtonVariant::Ghost,
                false,
                palette,
                rescan_click,
            )),
        )
        .child(rows);

    if let Some(note) = medium_note_div(flow.disk_medium_note(), locale, palette) {
        container = container.child(note);
    }

    container
}

/// Which text (if any) `steps::disk_select` should show below the candidate
/// list for a given `LiveMediumNote` — pulled into its own pure function
/// (Y20-P6) so this round's review finding ("Failed renders the warning")
/// is directly unit-testable without a live `gpui` window, same "extract
/// the pure decision, test IT" discipline `steps::progress::running_label`
/// already established for its own sibling bug fix. `LiveMediumNote::None`
/// renders nothing — see that variant's own doc comment in `state.rs`.
fn medium_note_text(note: &LiveMediumNote, locale: Locale) -> Option<String> {
    match note {
        LiveMediumNote::None => None,
        LiveMediumNote::Excluded(name) => Some(t1(locale, Key::LiveInstallDiskExcludedMedium, name)),
        LiveMediumNote::DetectionFailed => Some(t(locale, Key::LiveInstallDiskMediumUnknown).to_string()),
    }
}

/// Thin gpui wrapper over `medium_note_text` — `DetectionFailed` renders in
/// `palette.warning` (a caution, not a hard error: the mount-based backstop
/// in `filter_install_targets` is still independently protecting the
/// operator even when this note can't name the device), while `Excluded`
/// keeps the same plain muted color every other status line in this file
/// uses.
fn medium_note_div(note: &LiveMediumNote, locale: Locale, palette: ShellPalette) -> Option<Div> {
    let is_failed = matches!(note, LiveMediumNote::DetectionFailed);
    let text = medium_note_text(note, locale)?;
    let color = if is_failed { palette.warning } else { palette.muted_foreground };
    Some(div().w_full().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text))
}

fn disk_row(disk: &DiskInfo, index: usize, selected: bool, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let disk_for_click = disk.clone();
    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.live_install.as_mut() {
            flow.select_disk(disk_for_click.clone());
        }
        cx.notify();
    });

    let mut row = div()
        .id(("live-install-disk", index))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(if selected { palette.secondary } else { palette.surface }, 1.0))
        .border_1()
        .border_color(if selected { theme::alpha(palette.brand, 1.0) } else { palette.surface_border })
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(
            // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): flex
            // row main-axis `min-width:auto` overflow — this text column sits
            // beside the "已選取" tag inside a `justify_between` row, and without
            // `.flex_1().min_w(px(0.))` it refuses to shrink below `disk.model`'s
            // content width (lsblk model strings can be long). Template:
            // `settings/widgets.rs` `value_row`.
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).child(format!("/dev/{}", disk.name)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(disk_detail_line(disk))),
        )
        .on_click(on_click);

    if selected {
        row = row.child(
            div()
                // Companion to the text column's `.flex_1()` above: pins the tag to
                // its content width so it can never be squeezed by the now-growing
                // text column on the other side of `justify_between`.
                .flex_none()
                .text_size(px(theme::TEXT_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(palette.success, 1.0))
                .child("已選取 · Selected"),
        );
    }
    row
}

fn disk_detail_line(disk: &DiskInfo) -> String {
    if disk.model.is_empty() {
        disk.size.clone()
    } else {
        format!("{}  ·  {}", disk.size, disk.model)
    }
}

/// Kicks off a background `lsblk` scan and bridges its result back to
/// `ShellView` — same background-thread -> `std::sync::mpsc` -> `cx.spawn`
/// poll-loop pattern `oobe::steps::network`'s own `kick_off_scan` already
/// established (see that fn's own header comment).
fn kick_off_scan(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if let Some(flow) = view.live_install.as_mut() {
        flow.set_disk_scanning();
    }
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = scan_disks();
        let _ = tx.send(result);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    if let Some(flow) = view.live_install.as_mut() {
                        match result {
                            Ok((disks, note)) => flow.set_disk_scan_loaded(disks, note),
                            Err(message) => flow.set_disk_scan_failed(message),
                        }
                    }
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
    })
    .detach();
}

/// Real `lsblk` call — `Err` (binary missing, non-zero exit, unreadable/
/// malformed output) is a legitimate, disclosed failure, never silently
/// coerced to an empty list (`DiskScanState::Loaded(vec![])` means "asked,
/// got zero candidates"; `Failed` means "could not even ask" — the
/// empty-list state still gets its own honest empty panel above, distinct
/// from this one).
///
/// Y20-P6: the `lsblk` invocation itself changed from P3/P5's `-dno
/// NAME,TYPE,SIZE,RO,MODEL` (top-level disks only, plain text) to `-J -o
/// NAME,TYPE,SIZE,RO,MODEL,MOUNTPOINTS` (the full device tree, JSON) — see
/// this file's own header comment for why: the mount-based exclusion needs
/// each disk's CHILDREN, which a flat per-disk text line can't represent.
fn scan_disks() -> Result<(Vec<DiskInfo>, LiveMediumNote), String> {
    let output = std::process::Command::new("lsblk")
        .args(["-J", "-o", "NAME,TYPE,SIZE,RO,MODEL,MOUNTPOINTS"])
        .output()
        .map_err(|e| format!("無法執行 lsblk：{e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("lsblk 結束碼異常：{:?}：{}", output.status.code(), stderr.trim()));
    }
    let rows = parse_lsblk_json(&String::from_utf8_lossy(&output.stdout))?;
    let detection = find_live_medium_device();
    let live_medium_name = match &detection {
        LiveMediumDetection::Found(name) => Some(name.as_str()),
        LiveMediumDetection::NotFound | LiveMediumDetection::Failed(_) => None,
    };
    let (disks, excluded) = filter_install_targets(&rows, live_medium_name);
    let note = match detection {
        LiveMediumDetection::Found(_) => excluded.map(LiveMediumNote::Excluded).unwrap_or(LiveMediumNote::None),
        LiveMediumDetection::NotFound => LiveMediumNote::None,
        LiveMediumDetection::Failed(_reason) => LiveMediumNote::DetectionFailed,
    };
    Ok((disks, note))
}

/// One row this file's own filter step (`filter_install_targets`) actually
/// needs — flattened out of `lsblk -J`'s own nested tree by
/// `parse_lsblk_json` below. Distinct from `DiskInfo` (the already-filtered,
/// UI-facing shape `state.rs` owns) because filtering needs
/// `kind`/`read_only`/`has_mounted_filesystem` too, none of which `DiskInfo`
/// has any use for once a row has passed the gate.
#[derive(Debug, Clone, PartialEq)]
struct LsblkRow {
    name: String,
    kind: String,
    size: String,
    read_only: bool,
    model: String,
    /// Y20-P6 §(a): true if THIS device, or ANY descendant (a partition, or
    /// deeper), currently has a non-null, non-empty mountpoint —
    /// filename-independent, see `filter_install_targets`'s own doc comment
    /// for why this exclusion exists ALONGSIDE (not instead of)
    /// `live_medium_device`.
    has_mounted_filesystem: bool,
}

/// `lsblk -J -o NAME,TYPE,SIZE,RO,MODEL,MOUNTPOINTS`'s own per-device JSON
/// shape — `children` holds partitions (and, for LVM/dm devices, deeper
/// nesting; not a concern for the disk-only candidate list this file
/// builds, but kept recursive so ANY descendant's mountpoint still counts,
/// not just a direct child's). `name`/`type` have no `#[serde(default)]` —
/// a `blockdevices` entry missing either is malformed enough that failing
/// the WHOLE parse (see `parse_lsblk_json`'s own doc comment) is more
/// honest than silently guessing at a placeholder.
#[derive(Debug, Clone, Deserialize)]
struct LsblkNode {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<String>,
    /// lsblk's own boolean-ish columns have changed JSON representation
    /// across util-linux releases (a real JSON `true`/`false` on current
    /// versions; a `"0"`/`"1"` string on some older ones) — `deserialize_
    /// flexible_bool` below accepts either rather than this struct picking
    /// one shape and failing the whole parse on a build that emits the
    /// other (this crate can't pin which util-linux the live target image
    /// ships).
    #[serde(default, deserialize_with = "deserialize_flexible_bool")]
    ro: bool,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    mountpoints: Vec<Option<String>>,
    #[serde(default)]
    children: Vec<LsblkNode>,
}

#[derive(Debug, Clone, Deserialize)]
struct LsblkReport {
    blockdevices: Vec<LsblkNode>,
}

fn deserialize_flexible_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        serde_json::Value::Number(n) => n.as_i64() == Some(1),
        _ => false,
    })
}

/// Pure, unit-testable parser for `lsblk -J -o NAME,TYPE,SIZE,RO,MODEL,
/// MOUNTPOINTS`'s stdout — Y20-P6 replaces the P3/P5-era hand-rolled
/// whitespace-split text parser with real JSON decoding: the mount-based
/// exclusion (§(a) in this file's own header comment) needs each disk's
/// CHILDREN too, and a nested tree is not something a flat `NAME TYPE SIZE
/// RO MODEL` text line can represent at all. Malformed JSON, or a
/// `blockdevices` entry missing a required field, is a disclosed `Err`
/// (surfaces as `DiskScanState::Failed`, exactly like a non-zero `lsblk`
/// exit already does), never a silently-empty or silently-partial candidate
/// list.
fn parse_lsblk_json(raw: &str) -> Result<Vec<LsblkRow>, String> {
    let report: LsblkReport = serde_json::from_str(raw).map_err(|e| format!("無法解析 lsblk 輸出：{e}"))?;
    Ok(report.blockdevices.iter().map(lsblk_node_to_row).collect())
}

fn lsblk_node_to_row(node: &LsblkNode) -> LsblkRow {
    LsblkRow {
        name: node.name.clone(),
        kind: node.kind.clone(),
        size: node.size.clone().unwrap_or_default(),
        read_only: node.ro,
        model: node.model.clone().unwrap_or_default(),
        has_mounted_filesystem: node_has_any_mountpoint(node),
    }
}

/// True if `node` ITSELF has a non-null, non-empty mountpoint, or ANY
/// descendant does — recursive so a disk with an unmounted first partition
/// but a mounted second one (or a mounted sub-partition several levels
/// down) is still correctly flagged.
fn node_has_any_mountpoint(node: &LsblkNode) -> bool {
    let self_mounted = node.mountpoints.iter().any(|mp| mp.as_deref().is_some_and(|s| !s.is_empty()));
    self_mounted || node.children.iter().any(node_has_any_mountpoint)
}

fn is_excluded_name(name: &str) -> bool {
    ["loop", "ram", "sr", "fd"].iter().any(|prefix| name.starts_with(prefix))
}

/// The pure filter step. Five independent exclusion reasons, checked in
/// this order:
///
///   1. `live_medium_device` match — checked FIRST and unconditionally,
///      before anything else about the row is looked at, so the live medium
///      is excluded (and reported via the second return value) regardless
///      of what its own row otherwise looks like. This is the original
///      Y20-P5 fix for the screens/05-installer-disks.png bug: a
///      virtio-attached ISO enumerates as `TYPE=disk`, not `rom`, so
///      nothing about ITS OWN row would ever have excluded it without this
///      check.
///   2. `TYPE != "disk"` (catches `rom`/`part`/etc. — TYPE-based, not
///      merely name-prefix, so a future kernel/lsblk quirk misclassifying a
///      name can't slip a loop/ram device through).
///   3. `is_excluded_name` (defense in depth, unchanged from P3 — real
///      `lsblk` output never actually types `loop*`/`ram*`/`sr*`/`fd*` as
///      `disk`, but this closes that gap anyway).
///   4. `read_only` (Y20-P5: `RO=1` from lsblk's own flag — catches a
///      read-only medium `lsblk` happens to type as `disk`, independent of
///      name or the live-medium check above).
///   5. `has_mounted_filesystem` (Y20-P6 §(a), NEW this round: any disk (or
///      any of its partitions) that is CURRENTLY mounted is never a fresh
///      install target — filename-independent, so it still protects a
///      genuinely-mounted live medium even when `live_medium_device` itself
///      is `None` (detection failed, returned `NotFound`, or simply hasn't
///      run) — the whole point of this round's HIGH-severity fix. Folded
///      into the plain candidate exclusion, NOT also recorded as
///      `excluded_live_medium`: this check has no way to know FOR CERTAIN a
///      mounted disk IS the live medium specifically (it could be some
///      other mounted volume), so it excludes without asserting that
///      specific label.
///
/// Returns the filtered candidate list plus, separately, whether
/// `live_medium_device` was actually present among `rows` at all (`None`
/// when either `live_medium_device` itself is `None`, or it never matched
/// any row in this particular scan).
fn filter_install_targets(rows: &[LsblkRow], live_medium_device: Option<&str>) -> (Vec<DiskInfo>, Option<String>) {
    let mut disks = Vec::new();
    let mut excluded_live_medium = None;

    for row in rows {
        if live_medium_device == Some(row.name.as_str()) {
            excluded_live_medium = Some(row.name.clone());
            continue;
        }
        if row.kind != "disk" {
            continue;
        }
        if is_excluded_name(&row.name) {
            continue;
        }
        if row.read_only {
            continue;
        }
        if row.has_mounted_filesystem {
            continue;
        }
        disks.push(DiskInfo { name: row.name.clone(), size: row.size.clone(), model: row.model.clone() });
    }

    (disks, excluded_live_medium)
}

/// Y20-P6: the tri-state result of "which physical disk is the live
/// medium" detection — see this file's own header comment ("Y20-P6 (b)")
/// for why `Option<String>` was replaced. Private to this file: converted
/// into `state::LiveMediumNote` (the PERSISTED, cross-file-visible shape)
/// by `scan_disks` before it ever reaches `LiveInstallFlow`.
enum LiveMediumDetection {
    /// Positively identified — carries the same bare device basename shape
    /// `Option<String>` used to.
    Found(String),
    /// The directory search itself completed cleanly (every scanned
    /// directory that exists was readable) but none contained
    /// `duduclaw-install.wic.zst` — a legitimate "no live medium under any
    /// of these paths" outcome. NOT surfaced to the operator:
    /// `filter_install_targets`'s own mount-based exclusion (§(a) in this
    /// file's own header comment) is filename-independent and still
    /// protects a genuinely-mounted live medium regardless of whether this
    /// fn can name it.
    NotFound,
    /// The install image WAS located under one of the scanned directories
    /// (proof a live medium IS present), but resolving it to a physical
    /// device did not succeed — `findmnt`/`lsblk` missing, a non-zero exit,
    /// or empty output where a real answer was expected. The one outcome
    /// that must be surfaced: see `state::LiveMediumNote::DetectionFailed`.
    Failed(String),
}

/// Re-derives `duduclaw-os-install.sh`'s own §1 "which physical disk
/// carries the live medium" search (see that script's own header comment)
/// so `filter_install_targets` above can exclude it BY NAME even when it
/// looks like an ordinary disk. Two steps, exactly mirroring the script:
///
///   1. `find_live_medium_mount` — find which mounted directory carries the
///      install payload (`/media/realroot` first — the guaranteed path once
///      `image-live.bbclass`'s `init-live.sh` has moved the live root past
///      the initramfs phase, same reasoning the script's own §1 comment
///      documents; `/run/media/*` and `/media/*` as fallbacks for other live
///      topologies, e.g. a USB medium udev-extraconf auto-mounts). `None`
///      here becomes `LiveMediumDetection::NotFound` — see that variant's
///      own doc comment for why this is NOT treated as a failure.
///   2. Resolve that mount back to a physical block device: `findmnt -no
///      SOURCE --target <dir>` gives the partition/device backing the
///      mount, then `lsblk -no PKNAME <source>` gives its parent disk. An
///      empty `PKNAME` (optical media has no partition table — the device
///      itself, e.g. `/dev/sr0`, IS the disk) falls back to the source's own
///      basename, identical to the script's `[ -n "$SRC_DISK" ] ||
///      SRC_DISK="$(basename "$SRC_PART")"` — this is a normal, EXPECTED
///      outcome, not a failure. A genuine `findmnt`/`lsblk` failure at
///      either step (missing binary, non-zero exit, no output where one was
///      expected) becomes `LiveMediumDetection::Failed` instead — Y20-P6's
///      whole point: step 1 already proved a live medium exists, so failing
///      to name its device now must be VISIBLE, not silently degraded to
///      "exclude nothing extra" the way this fn used to (`Option<String>`
///      collapsed this into the same `None` `NotFound` uses).
fn find_live_medium_device() -> LiveMediumDetection {
    const INSTALL_IMAGE_NAME: &str = "duduclaw-install.wic.zst";
    let Some(mount_dir) = find_live_medium_mount(INSTALL_IMAGE_NAME) else {
        return LiveMediumDetection::NotFound;
    };
    let source = match findmnt_source(&mount_dir) {
        Ok(source) => source,
        Err(reason) => return LiveMediumDetection::Failed(reason),
    };
    match resolve_parent_disk(&source) {
        Ok(name) => LiveMediumDetection::Found(name),
        Err(reason) => LiveMediumDetection::Failed(reason),
    }
}

/// Searches `/media/realroot` (checked first, unconditionally — matches the
/// script's own priority), then every subdirectory of `/run/media` and
/// `/media`, for a directory containing `install_image_name` directly.
fn find_live_medium_mount(install_image_name: &str) -> Option<PathBuf> {
    std::iter::once(PathBuf::from("/media/realroot"))
        .chain(subdirs_of("/run/media"))
        .chain(subdirs_of("/media"))
        .find(|dir| dir.join(install_image_name).is_file())
}

fn subdirs_of(parent: &str) -> Vec<PathBuf> {
    std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// `findmnt -n -o SOURCE --target <dir>` — the block device (or partition)
/// backing whatever filesystem is mounted at `dir`. Y20-P6: `Result`, not
/// `Option` — a spawn failure, non-zero exit, or empty output here is now a
/// `Failed` case (see `find_live_medium_device`'s own doc comment), never a
/// silent `None`.
fn findmnt_source(dir: &Path) -> Result<String, String> {
    let output = std::process::Command::new("findmnt")
        .args(["-n", "-o", "SOURCE", "--target"])
        .arg(dir)
        .output()
        .map_err(|e| format!("無法執行 findmnt：{e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("findmnt 結束碼異常：{:?}：{}", output.status.code(), stderr.trim()));
    }
    let source = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if source.is_empty() {
        Err(format!("findmnt 對 {} 沒有回傳來源裝置", dir.display()))
    } else {
        Ok(source)
    }
}

/// `lsblk -no PKNAME <source>` — the bare basename of `source`'s parent disk
/// (e.g. `"vda"` for a `/dev/vda1` partition). An EMPTY `PKNAME` (optical
/// media: the device itself has no partition table, so it IS the disk) is
/// the normal, expected case and still falls back to `source`'s own
/// basename via `Ok` — only a genuine `lsblk` failure (missing binary,
/// non-zero exit) is `Err` (Y20-P6: this used to silently fall back to the
/// same basename guess even when `lsblk` itself couldn't run at all, which
/// could name the WRONG device — e.g. a partition's own name when the
/// intent was always its parent disk).
fn resolve_parent_disk(source: &str) -> Result<String, String> {
    let output = std::process::Command::new("lsblk")
        .args(["-no", "PKNAME"])
        .arg(source)
        .output()
        .map_err(|e| format!("無法執行 lsblk -no PKNAME：{e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("lsblk -no PKNAME 結束碼異常：{:?}：{}", output.status.code(), stderr.trim()));
    }
    let pkname = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if pkname.is_empty() {
        Ok(source.rsplit('/').next().unwrap_or(source).to_string())
    } else {
        Ok(pkname)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_lsblk_json ──────────────────────────────────────────────

    #[test]
    fn parses_a_single_qemu_virtio_disk_from_json() {
        let raw = r#"{"blockdevices":[{"name":"vda","type":"disk","size":"20G","ro":false,"model":"QEMU HARDDISK","mountpoints":[null],"children":[]}]}"#;
        let rows = parse_lsblk_json(raw).expect("valid JSON must parse");
        assert_eq!(rows, vec![LsblkRow {
            name: "vda".to_string(),
            kind: "disk".to_string(),
            size: "20G".to_string(),
            read_only: false,
            model: "QEMU HARDDISK".to_string(),
            has_mounted_filesystem: false,
        }]);
    }

    #[test]
    fn ro_true_parses_as_read_only() {
        let raw = r#"{"blockdevices":[{"name":"vdb","type":"disk","size":"1.8G","ro":true,"model":"Virtual_CD","mountpoints":[null]}]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert!(rows[0].read_only);
    }

    /// Defensive: some util-linux releases emit `"ro"` as a string
    /// (`"0"`/`"1"`) rather than a real JSON boolean — `deserialize_
    /// flexible_bool` must accept either, since this crate can't pin which
    /// util-linux the live target image ships.
    #[test]
    fn ro_as_a_legacy_string_still_parses_as_read_only() {
        let raw = r#"{"blockdevices":[{"name":"vdb","type":"disk","size":"1.8G","ro":"1","model":null,"mountpoints":[null]}]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert!(rows[0].read_only);
    }

    #[test]
    fn a_model_with_no_value_is_an_empty_string_not_missing() {
        let raw = r#"{"blockdevices":[{"name":"vda","type":"disk","size":"20G","ro":false,"model":null,"mountpoints":[null]}]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert_eq!(rows[0].model, "");
    }

    #[test]
    fn malformed_json_is_a_disclosed_error_not_a_panic() {
        assert!(parse_lsblk_json("not json at all").is_err());
    }

    #[test]
    fn a_blockdevice_missing_a_required_field_is_a_disclosed_error() {
        // `type` has no `#[serde(default)]` on `LsblkNode` — an entry
        // missing it fails the WHOLE parse rather than silently guessing a
        // placeholder TYPE (see `LsblkNode`'s own doc comment).
        assert!(parse_lsblk_json(r#"{"blockdevices":[{"name":"vda"}]}"#).is_err());
    }

    #[test]
    fn empty_blockdevices_yields_no_rows() {
        let rows = parse_lsblk_json(r#"{"blockdevices":[]}"#).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn multiple_real_disks_all_parse() {
        let raw = r#"{"blockdevices":[
            {"name":"sda","type":"disk","size":"500G","ro":false,"model":"Samsung_SSD","mountpoints":[null]},
            {"name":"sdb","type":"disk","size":"1T","ro":false,"model":"Seagate_HDD","mountpoints":[null]}
        ]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "sda");
        assert_eq!(rows[1].name, "sdb");
    }

    // ── node_has_any_mountpoint / has_mounted_filesystem (Y20-P6 §(a)) ──

    #[test]
    fn a_partition_mountpoint_marks_the_parent_disk_as_mounted() {
        let raw = r#"{"blockdevices":[{"name":"vdb","type":"disk","size":"1.8G","ro":false,"model":"Virtual_stick",
            "mountpoints":[null],
            "children":[{"name":"vdb1","type":"part","size":"1.8G","ro":false,"model":null,"mountpoints":["/media/realroot"]}]
        }]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert!(rows[0].has_mounted_filesystem, "a mounted PARTITION must mark its parent disk as mounted too");
    }

    #[test]
    fn a_disk_mounted_directly_is_marked_mounted() {
        // Optical media: the whole device is the mount source, no partition
        // table at all.
        let raw = r#"{"blockdevices":[{"name":"sr0","type":"rom","size":"1024M","ro":true,"model":"CD-ROM","mountpoints":["/media/realroot"],"children":[]}]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert!(rows[0].has_mounted_filesystem);
    }

    #[test]
    fn an_unmounted_disk_with_unmounted_partitions_is_not_marked_mounted() {
        let raw = r#"{"blockdevices":[{"name":"vda","type":"disk","size":"20G","ro":false,"model":"QEMU HARDDISK",
            "mountpoints":[null],
            "children":[{"name":"vda1","type":"part","size":"20G","ro":false,"model":null,"mountpoints":[null]}]
        }]}"#;
        let rows = parse_lsblk_json(raw).unwrap();
        assert!(!rows[0].has_mounted_filesystem);
    }

    // ── filter_install_targets (Y20-P5, extended Y20-P6) ────────────────

    fn row(name: &str, kind: &str, size: &str, read_only: bool, model: &str, has_mounted_filesystem: bool) -> LsblkRow {
        LsblkRow { name: name.to_string(), kind: kind.to_string(), size: size.to_string(), read_only, model: model.to_string(), has_mounted_filesystem }
    }

    /// (a) rom device excluded.
    #[test]
    fn rom_device_is_excluded() {
        let rows = vec![row("sr0", "rom", "1024M", false, "CD-ROM", false), row("vda", "disk", "20G", false, "QEMU HARDDISK", false)];
        let (disks, excluded) = filter_install_targets(&rows, None);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
        assert_eq!(excluded, None, "no live medium was named, so nothing should be reported as excluded-as-medium");
    }

    /// (b) a read-only device is excluded even though its own TYPE/name
    /// look like an ordinary disk.
    #[test]
    fn read_only_device_is_excluded() {
        let rows = vec![row("vdb", "disk", "1.8G", true, "Virtual_CD", false), row("vda", "disk", "20G", false, "QEMU HARDDISK", false)];
        let (disks, _excluded) = filter_install_targets(&rows, None);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
    }

    /// (c) the live-medium device is excluded even when it looks exactly
    /// like a normal disk (the USB-stick / virtio-CD case: `TYPE=disk`,
    /// `RO=0`, unmounted per lsblk's own flag, nothing about its own row
    /// distinguishes it) — this is the exact screens/05-installer-disks.png
    /// regression Y20-P5 fixed.
    #[test]
    fn live_medium_device_is_excluded_even_when_it_looks_like_a_normal_disk() {
        let rows = vec![row("vdb", "disk", "1.8G", false, "Virtual_CD", false), row("vda", "disk", "32G", false, "QEMU HARDDISK", false)];
        let (disks, excluded) = filter_install_targets(&rows, Some("vdb"));
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
        assert_eq!(excluded, Some("vdb".to_string()));
    }

    /// (d) an ordinary disk, with no live medium named at all, is kept.
    #[test]
    fn an_ordinary_disk_is_kept() {
        let rows = vec![row("vda", "disk", "32G", false, "QEMU HARDDISK", false)];
        let (disks, excluded) = filter_install_targets(&rows, None);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
        assert_eq!(disks[0].size, "32G");
        assert_eq!(disks[0].model, "QEMU HARDDISK");
        assert_eq!(excluded, None);
    }

    /// Y20-P6 §(a), review-finding test 1: a disk with a mounted filesystem
    /// is excluded even with NO live medium named at all (detection
    /// returned `NotFound`/`Failed`, or simply never ran) — the
    /// filename-independent backstop this round's HIGH fix adds.
    #[test]
    fn mounted_disk_is_excluded_even_without_a_named_live_medium() {
        let rows = vec![row("vdb", "disk", "1.8G", false, "Virtual_stick", true), row("vda", "disk", "32G", false, "QEMU HARDDISK", false)];
        let (disks, excluded) = filter_install_targets(&rows, None);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
        assert_eq!(excluded, None, "the mount-based exclusion is silent — it never asserts the specific 已排除安裝媒介 label");
    }

    /// Y20-P6, review-finding test 2: an ordinary target disk with NO
    /// mounted filesystem anywhere on it (or its partitions) is kept — the
    /// mount-based check must not over-exclude a disk that simply has an
    /// old, currently-unmounted partition table from a previous install.
    #[test]
    fn target_with_no_mounts_is_kept() {
        let rows = vec![row("vda", "disk", "32G", false, "QEMU HARDDISK", false)];
        let (disks, _excluded) = filter_install_targets(&rows, None);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
    }

    #[test]
    fn loop_ram_optical_and_floppy_devices_are_all_excluded() {
        let rows = vec![
            row("loop0", "loop", "100M", false, "", false),
            row("ram0", "disk", "64M", false, "", false),
            row("sr0", "rom", "1024M", false, "CD-ROM", false),
            row("fd0", "disk", "1.4M", false, "", false),
            row("vda", "disk", "20G", false, "QEMU HARDDISK", false),
        ];
        let (disks, _excluded) = filter_install_targets(&rows, None);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
    }

    #[test]
    fn ram_prefixed_type_disk_is_still_excluded_by_name() {
        // Defense-in-depth per `is_excluded_name`'s own doc comment: even if
        // TYPE says "disk", a ram*-prefixed name is excluded.
        let rows = vec![row("ram0", "disk", "64M", false, "", false)];
        let (disks, _) = filter_install_targets(&rows, None);
        assert!(disks.is_empty());
    }

    #[test]
    fn a_non_disk_type_is_excluded_even_with_an_allowed_name() {
        let rows = vec![row("vda", "part", "20G", false, "", false)];
        let (disks, _) = filter_install_targets(&rows, None);
        assert!(disks.is_empty());
    }

    /// A live-medium name that never actually shows up among `rows` (the
    /// detection step found something, but this particular scan's rows
    /// don't contain it — e.g. a race between the two separate `lsblk`
    /// calls) reports no exclusion note rather than a misleading one.
    #[test]
    fn a_live_medium_name_not_present_in_rows_reports_no_exclusion() {
        let rows = vec![row("vda", "disk", "32G", false, "QEMU HARDDISK", false)];
        let (disks, excluded) = filter_install_targets(&rows, Some("vdc"));
        assert_eq!(disks.len(), 1);
        assert_eq!(excluded, None);
    }

    // ── medium_note_text (Y20-P6 §(b) + LOW i18n finding) ───────────────

    /// Review-finding test 3: "Failed renders the warning".
    #[test]
    fn detection_failed_renders_the_choose_carefully_warning() {
        let text = medium_note_text(&LiveMediumNote::DetectionFailed, Locale::ZhTw).expect("Failed must render a note");
        assert_eq!(text, t(Locale::ZhTw, Key::LiveInstallDiskMediumUnknown));
        assert!(!text.contains("已排除"), "must not reuse the Excluded wording — the device was never actually named");
    }

    #[test]
    fn excluded_renders_the_device_specific_note() {
        let text = medium_note_text(&LiveMediumNote::Excluded("vdb".to_string()), Locale::ZhTw).expect("Excluded must render a note");
        assert!(text.contains("vdb"), "{text}");
    }

    #[test]
    fn none_renders_no_note() {
        assert_eq!(medium_note_text(&LiveMediumNote::None, Locale::ZhTw), None);
    }

    /// Pins the English wording too (Y20-P6, LOW finding: these two strings
    /// used to be a fixed bilingual literal; now they genuinely vary by
    /// locale) — `i18n/tests.rs`'s own completeness test only checks
    /// non-emptiness, not wording, so a future edit that only updates the
    /// zh-TW catalog string couldn't be caught there.
    #[test]
    fn medium_notes_read_correctly_in_english_too() {
        assert_eq!(medium_note_text(&LiveMediumNote::Excluded("vdb".to_string()), Locale::En), Some("Install medium excluded: /dev/vdb".to_string()));
        assert_eq!(
            medium_note_text(&LiveMediumNote::DetectionFailed, Locale::En),
            Some("Could not identify the install medium — choose carefully".to_string())
        );
    }

    // ── findmnt_source / resolve_parent_disk (Y20-P6: now Result, not
    // silently-falling-back Option) ──────────────────────────────────────

    #[test]
    fn findmnt_source_reports_failure_when_it_cannot_run() {
        // Can't stub `findmnt` itself — but on a machine with no `findmnt`
        // on `$PATH` at all (this crate's own macOS dev target, see
        // `Cargo.toml`'s own doc comment on why this crate builds there for
        // dev use even though it only ever RUNS on the Linux kiosk target),
        // or one where the target simply isn't a mountpoint, `Command::new
        // ("findmnt")` fails to spawn or exits non-zero either way — both
        // land in this `Err` branch (Y20-P6: this must now be visible, not
        // a silent `None`).
        assert!(findmnt_source(Path::new("/definitely/not/a/real/mount/dir")).is_err());
    }

    #[test]
    fn resolve_parent_disk_reports_failure_when_lsblk_cannot_run() {
        // Same reasoning as `findmnt_source`'s own test above — Y20-P6: a
        // genuine `lsblk` failure must surface as `Err`, never silently
        // fall back to a possibly-wrong basename guess.
        assert!(resolve_parent_disk("/dev/definitely-not-a-real-device-xyz").is_err());
    }
}
