// Y20-P3 (2026-08-29) — real block-device enumeration + single-select pick,
// replacing the P2 "開發中" placeholder. Mirrors `oobe::steps::network`'s
// scan shape end to end: `DiskScanState::NotScanned` (an explicit "掃描磁碟"
// button — I/O is always click-triggered in this crate, never a render-time
// side effect, same discipline that file's own header comment documents) ->
// `Scanning` -> `Loaded`/`Failed`; `Loaded` with zero entries renders its
// own honest empty state.
//
// The candidate list mirrors `duduclaw-os-install.sh`'s own CANDIDATES
// enumeration (§2 as of Y19: `lsblk -dno NAME,TYPE` filtered to
// `TYPE=="disk"`, `loop*`/`ram*`/`sr*`/`fd*` dropped) — this file asks for
// `SIZE,MODEL` in the same call so the UI can show a human a size/model
// hint the script itself never needed. See `DiskInfo`'s own doc comment
// (`state.rs`) for the one simplification this side does NOT re-derive (the
// install medium's own source-disk exclusion) and why the shell script's
// own CANDIDATES check is still the fail-closed backstop for that gap.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::oobe::widgets;
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::{DiskInfo, DiskScanState, LiveInstallFlow};

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
                .child("找不到可安裝的磁碟 · No installable disk found"),
        )
        .child(widgets::step_button("live-install-disk-rescan", "重新整理 · Rescan", widgets::StepButtonVariant::Secondary, false, palette, click))
}

fn loaded_panel(disks: &[DiskInfo], flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();
    let selected = flow.selected_disk().map(|d| d.name.clone());

    let rescan_click = cx.listener(|view, _ev, _window, cx| kick_off_scan(view, cx));

    let mut rows = div().flex().flex_col().gap(px(6.));
    for (index, disk) in disks.iter().enumerate() {
        rows = rows.child(disk_row(disk, index, selected.as_deref() == Some(disk.name.as_str()), palette, cx));
    }

    div()
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
        .child(rows)
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
                            Ok(disks) => flow.set_disk_scan_loaded(disks),
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

/// Real `lsblk` call — `Err` (binary missing, non-zero exit, unreadable
/// output) is a legitimate, disclosed failure, never silently coerced to an
/// empty list (`DiskScanState::Loaded(vec![])` means "asked, got zero
/// candidates"; `Failed` means "could not even ask" — the empty-list state
/// still gets its own honest empty panel above, distinct from this one).
fn scan_disks() -> Result<Vec<DiskInfo>, String> {
    let output = std::process::Command::new("lsblk")
        .args(["-dno", "NAME,TYPE,SIZE,MODEL"])
        .output()
        .map_err(|e| format!("無法執行 lsblk：{e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("lsblk 結束碼異常：{:?}：{}", output.status.code(), stderr.trim()));
    }
    Ok(parse_lsblk_disks(&String::from_utf8_lossy(&output.stdout)))
}

/// Pure, unit-testable parser for `lsblk -dno NAME,TYPE,SIZE,MODEL`'s
/// stdout — same "hand-rolled parser as its own free function, exercised by
/// a table of raw strings" shape `audio/wpctl.rs`'s parsers establish.
/// Mirrors `duduclaw-os-install.sh`'s own CANDIDATES filter (see this
/// file's own header comment): `TYPE` column must read `disk`, and
/// `loop`/`ram`/`sr`/`fd`-prefixed names are excluded regardless of `TYPE`
/// — defense in depth (real `lsblk` output never actually types those as
/// `disk`, but a future kernel/lsblk quirk misclassifying one must not
/// silently offer a RAM disk or loop device as an install target).
fn parse_lsblk_disks(raw: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(kind) = parts.next() else { continue };
        if kind != "disk" {
            continue;
        }
        if is_excluded_name(name) {
            continue;
        }
        let size = parts.next().unwrap_or_default().to_string();
        let model = parts.collect::<Vec<_>>().join(" ");
        disks.push(DiskInfo { name: name.to_string(), size, model });
    }
    disks
}

fn is_excluded_name(name: &str) -> bool {
    ["loop", "ram", "sr", "fd"].iter().any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_qemu_virtio_disk() {
        let raw = "vda   disk  20G  QEMU HARDDISK\n";
        let disks = parse_lsblk_disks(raw);
        assert_eq!(disks, vec![DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: "QEMU HARDDISK".to_string() }]);
    }

    #[test]
    fn excludes_loop_ram_optical_and_floppy_devices() {
        let raw = "\
loop0 loop 100M
ram0  disk 64M
sr0   rom  1024M CD-ROM
fd0   disk 1.4M
vda   disk 20G QEMU HARDDISK
";
        let disks = parse_lsblk_disks(raw);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "vda");
    }

    #[test]
    fn a_model_with_no_value_is_an_empty_string_not_missing() {
        let raw = "vda disk 20G\n";
        let disks = parse_lsblk_disks(raw);
        assert_eq!(disks[0].model, "");
    }

    #[test]
    fn a_line_with_only_a_name_is_skipped_not_a_panic() {
        let raw = "vda\n";
        assert!(parse_lsblk_disks(raw).is_empty());
    }

    #[test]
    fn empty_input_yields_no_disks() {
        assert!(parse_lsblk_disks("").is_empty());
    }

    #[test]
    fn multiple_real_disks_all_parse() {
        let raw = "\
sda disk 500G Samsung_SSD
sdb disk 1T   Seagate_HDD
";
        let disks = parse_lsblk_disks(raw);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].name, "sda");
        assert_eq!(disks[1].name, "sdb");
    }

    #[test]
    fn ram_prefixed_type_disk_is_still_excluded() {
        // Defense-in-depth per this fn's own doc comment: even if TYPE says
        // "disk", a ram*-prefixed name is excluded.
        let raw = "ram0 disk 64M\n";
        assert!(parse_lsblk_disks(raw).is_empty());
    }

    #[test]
    fn a_non_disk_type_is_excluded_even_with_an_allowed_name() {
        let raw = "vda part 20G\n";
        assert!(parse_lsblk_disks(raw).is_empty());
    }
}
