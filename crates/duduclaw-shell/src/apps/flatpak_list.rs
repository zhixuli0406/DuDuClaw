// `flatpak list` enumeration — APP-1 (2026-08-22).
//
// The FLATPAK half of "what is actually installed on this machine" (the
// native half is `apps/desktop_entry.rs`). Pure argv builders + a pure
// output parser, so both can be pinned by tests on a machine that has no
// `flatpak` binary at all; `apps/installed.rs` is what actually runs the
// command.
//
// ── Why the columns are named explicitly ────────────────────────────────
// `flatpak list` with no `--columns` prints a DIFFERENT column set
// depending on version and on whether stdout is a terminal, and the header
// row it prints on a terminal is localized. Naming `--columns=application,
// name,origin` fixes both the set and the order, so this parser is reading
// a contract rather than guessing at a layout. Field separation is still
// tolerant of both shapes flatpak emits (tab-separated when piped, padded
// columns on a terminal) because THAT part is not something `--columns`
// pins — see `parse_list`.
//
// ── Why two invocations, not one ────────────────────────────────────────
// `apps.rs`'s install path documents (and a test pins) that every
// INSTALLING argv must carry `--installation=data`: the appliance's
// `/etc/flatpak/installations.d/10-duduclaw-data.conf` ADDS a named
// installation at `/data/flatpak`, it does not move the default one, and
// installing into the default would fill a 5 GB read-only-by-design root
// partition. That same file's own comment predicted enumeration would need
// the flag too.
//
// It does — but it is not SUFFICIENT, which is why `apps/installed.rs`
// runs BOTH `--installation=data` and a bare listing and merges them:
//   * `--installation=data` alone would miss anything in the system default
//     installation and anything a user installed with `--user`.
//   * a bare listing is documented to cover every configured installation,
//     which SHOULD include the extra one — but "should" is a claim about a
//     flatpak version this crate has never been able to run against
//     (no flatpak on the dev Mac, see `apps.rs`'s own honest-limitation
//     note), and the whole point of this work package is that the app list
//     must be real. Asking explicitly for the installation this appliance
//     actually installs into costs one extra subprocess per refresh and
//     removes the guess entirely.
// A failure of either one is not fatal to the other (a machine with no
// `data` installation simply gets a non-zero exit there) — see
// `apps/installed.rs::scan_flatpak`.

/// The exact column set `parse_list` below expects, in order.
const LIST_COLUMNS: &str = "--columns=application,name,origin";

/// Builds the `flatpak list` argv. `installation` is `Some("data")` for the
/// appliance's own extra installation, `None` for "every configured
/// installation" — see this file's header comment for why both are run.
///
/// `--app` excludes runtimes, extensions and locale packs: those are real
/// installed refs but they are not things a person launches, and listing
/// them would bury the actual apps under dozens of `org.freedesktop.
/// Platform.*` rows.
pub(crate) fn list_argv(installation: Option<&str>) -> Vec<String> {
    let mut argv = vec!["list".to_string(), "--app".to_string()];
    if let Some(name) = installation {
        argv.push(format!("--installation={name}"));
    }
    argv.push(LIST_COLUMNS.to_string());
    argv
}

/// One row of `flatpak list --columns=application,name,origin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatpakApp {
    /// The flatpak application id — also the argument `flatpak run` takes,
    /// and (by the freedesktop convention flatpak's own window integration
    /// relies on) the xdg-shell app_id its windows carry.
    pub app_id: String,
    /// The human-readable name flatpak reports. Falls back to `app_id` when
    /// the column is empty, so a row always has something to display.
    pub name: String,
    /// The remote it came from (`flathub`, …). `None` when flatpak printed
    /// nothing there.
    pub origin: Option<String>,
}

/// Parses `flatpak list` output. Unrecognisable lines are DROPPED, never
/// guessed at — a header row, a locale-translated header, a stray warning
/// on stdout, or an empty line all fail `is_plausible_app_id` and simply do
/// not become an app. This is what makes it safe not to pin whether a given
/// flatpak version prints a header.
pub(crate) fn parse_list(stdout: &str) -> Vec<FlatpakApp> {
    let mut out: Vec<FlatpakApp> = Vec::new();
    for line in stdout.lines() {
        let Some(fields) = split_row(line) else {
            continue;
        };
        let app_id = fields.first().map(|s| s.trim()).unwrap_or("");
        if !is_plausible_app_id(app_id) {
            continue;
        }
        let name = fields.get(1).map(|s| s.trim()).unwrap_or("");
        let origin = fields.get(2).map(|s| s.trim()).filter(|s| !s.is_empty());
        let app_id = app_id.to_string();
        out.push(FlatpakApp {
            name: if name.is_empty() { app_id.clone() } else { name.to_string() },
            app_id,
            origin: origin.map(|s| s.to_string()),
        });
    }
    out
}

/// Splits one output row into fields. Tab-separated is what flatpak emits
/// when stdout is a pipe (which is always the case here — `Command::output`
/// gives it a pipe); the two-or-more-spaces fallback covers the padded
/// table it prints on a terminal, so a human reproducing the command by
/// hand and pasting the output into a bug report still parses the same way.
fn split_row(line: &str) -> Option<Vec<String>> {
    let line = line.trim_end();
    if line.trim().is_empty() {
        return None;
    }
    if line.contains('\t') {
        return Some(line.split('\t').map(|f| f.trim().to_string()).collect());
    }
    let fields: Vec<String> = line.split("  ").map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
    if fields.is_empty() {
        return None;
    }
    Some(fields)
}

/// Whether a token can be a flatpak application id at all. Reverse-DNS
/// shaped: at least one dot, only `[A-Za-z0-9._-]`, and it may not start
/// with `-` (which would be read as a flag if it ever reached an argv) or
/// with a dot. Deliberately strict — this is the check that keeps a header
/// row, a translated column title or a warning line from becoming a fake
/// app in the Launcher.
pub(crate) fn is_plausible_app_id(value: &str) -> bool {
    if value.len() < 3 || !value.contains('.') {
        return false;
    }
    if value.starts_with('-') || value.starts_with('.') || value.ends_with('.') {
        return false;
    }
    value.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_installation_argv_is_pinned_exactly() {
        assert_eq!(
            list_argv(Some("data")),
            vec!["list".to_string(), "--app".to_string(), "--installation=data".to_string(), "--columns=application,name,origin".to_string()],
        );
    }

    #[test]
    fn the_all_installations_argv_omits_the_flag_entirely() {
        assert_eq!(list_argv(None), vec!["list".to_string(), "--app".to_string(), "--columns=application,name,origin".to_string()]);
    }

    /// The appliance-integration regression this crate already pins on the
    /// INSTALL side (`apps.rs::every_installing_argv_targets_the_data_
    /// installation`), asserted here for the ENUMERATION side too — an
    /// enumeration that silently drops the flag stops seeing everything
    /// this appliance ever installed.
    #[test]
    fn enumeration_can_target_the_appliances_own_installation() {
        assert!(list_argv(Some(crate::apps::FLATPAK_INSTALLATION)).contains(&"--installation=data".to_string()));
    }

    #[test]
    fn parses_tab_separated_output_the_shape_a_piped_flatpak_emits() {
        let out = "org.chromium.Chromium\tChromium\tflathub\norg.gnome.TextEditor\t文字編輯器\tflathub\n";
        assert_eq!(
            parse_list(out),
            vec![
                FlatpakApp { app_id: "org.chromium.Chromium".into(), name: "Chromium".into(), origin: Some("flathub".into()) },
                FlatpakApp { app_id: "org.gnome.TextEditor".into(), name: "文字編輯器".into(), origin: Some("flathub".into()) },
            ]
        );
    }

    #[test]
    fn parses_the_padded_table_a_terminal_flatpak_emits() {
        let out = "Application              Name       Origin\norg.chromium.Chromium    Chromium   flathub\n";
        let apps = parse_list(out);
        assert_eq!(apps.len(), 1, "the header row must not become an app");
        assert_eq!(apps[0].app_id, "org.chromium.Chromium");
        assert_eq!(apps[0].name, "Chromium");
        assert_eq!(apps[0].origin.as_deref(), Some("flathub"));
    }

    #[test]
    fn a_localized_header_row_is_dropped_too() {
        // The header is translated, so it can never be matched by name —
        // `is_plausible_app_id` is what actually rejects it.
        let out = "應用程式\t名稱\t來源\norg.chromium.Chromium\tChromium\tflathub\n";
        let apps = parse_list(out);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].app_id, "org.chromium.Chromium");
    }

    #[test]
    fn a_row_with_no_name_falls_back_to_the_app_id_rather_than_rendering_blank() {
        let apps = parse_list("org.example.App\t\t\n");
        assert_eq!(apps, vec![FlatpakApp { app_id: "org.example.App".into(), name: "org.example.App".into(), origin: None }]);
    }

    #[test]
    fn noise_on_stdout_never_becomes_an_app() {
        assert!(parse_list("").is_empty());
        assert!(parse_list("\n\n   \n").is_empty());
        assert!(parse_list("error: Installation data not found\n").is_empty());
        assert!(parse_list("Note: nothing to show\n").is_empty());
    }

    /// Live-fire against a REAL `flatpak` binary — never run by a bare
    /// `cargo test` (same `#[ignore]` contract this crate's other live tests
    /// use). This is the only way to check that the argv this module builds
    /// is actually accepted and that the output shape is actually what
    /// `parse_list` expects:
    ///
    /// ```text
    /// cargo test -- --ignored live_flatpak_list_against_real_flatpak --nocapture
    /// ```
    ///
    /// Skips (loudly, never silently passing as if it had run) when no
    /// `flatpak` binary is present.
    #[test]
    #[ignore]
    fn live_flatpak_list_against_real_flatpak() {
        for scope in [Some(crate::apps::FLATPAK_INSTALLATION), None] {
            let argv = list_argv(scope);
            eprintln!("[live] $ flatpak {}", argv.join(" "));
            match std::process::Command::new("flatpak").args(&argv).output() {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    eprintln!("[live]   exit={:?}", output.status.code());
                    eprintln!("[live]   stdout ({} bytes): {stdout:?}", stdout.len());
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stderr.trim().is_empty() {
                        eprintln!("[live]   stderr: {}", stderr.trim());
                    }
                    let parsed = parse_list(&stdout);
                    eprintln!("[live]   parsed {} row(s): {parsed:?}", parsed.len());
                    for row in &parsed {
                        assert!(is_plausible_app_id(&row.app_id));
                        assert!(!row.name.is_empty());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("[live] SKIPPED: no flatpak binary on this machine (this is the dev-Mac state, not a pass)");
                    return;
                }
                Err(e) => panic!("flatpak could not be run: {e}"),
            }
        }
    }

    #[test]
    fn app_id_plausibility_is_strict_about_shape() {
        assert!(is_plausible_app_id("org.chromium.Chromium"));
        assert!(is_plausible_app_id("io.github.some_app-2"));
        assert!(!is_plausible_app_id(""));
        assert!(!is_plausible_app_id("Chromium"), "no dot is not a reverse-DNS id");
        assert!(!is_plausible_app_id("-x.y"), "a flag-shaped token must never reach an argv");
        assert!(!is_plausible_app_id(".hidden.thing"));
        assert!(!is_plausible_app_id("org.example."));
        assert!(!is_plausible_app_id("應用.程式"), "non-ASCII is not a flatpak id — a translated header must not slip through");
        assert!(!is_plausible_app_id("org example.App"), "a space means the row was split wrong");
    }
}
