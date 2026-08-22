// XDG Desktop Entry parsing — APP-1 (2026-08-22).
//
// The NATIVE half of "what is actually installed on this machine". The
// appliance image ships real, non-flatpak GUI apps (chromium among them —
// verified on the VM), so a shell that only ever asked `flatpak list` would
// still be showing an incomplete picture. This module is the parser +
// directory-resolution half; `apps/installed.rs` does the actual filesystem
// walk and merges the result with the flatpak half.
//
// ── Hand-rolled, no new dependency ──────────────────────────────────────
// There ARE crates for this (`freedesktop-desktop-entry`, `ini`), and they
// were considered and rejected: this crate pins `gpui`/`gpui_platform`/
// `gpui_macros` to one exact zed monorepo rev precisely because a second
// resolved copy of gpui breaks the type identity the whole
// `duduclaw-native-gui` facade depends on (see `Cargo.toml`'s own header
// comment), so every new dependency edge here is a new chance for cargo's
// resolver to move something in that graph. The format itself is an
// INI-shaped, ASCII-keyed, five-escape-sequence grammar — the whole thing
// is under 200 lines of straight-line parsing with no lookahead — and the
// parts that actually matter for correctness (locale-suffixed keys, the
// `\;` list escape, field-code stripping in `Exec`) are exactly the parts a
// general-purpose INI crate would NOT do for us anyway. Spec followed:
// freedesktop.org Desktop Entry Specification 1.5.
//
// ── Deliberately pure ───────────────────────────────────────────────────
// Zero I/O and zero `std::env` reads in this file — every function takes
// its inputs (file content, an `XdgEnv` snapshot, a `PATH` directory list)
// as parameters, same "testable without a live window / live machine"
// discipline `surface.rs` / `install_gate.rs` / `notifications_feed.rs`
// already state for themselves. `apps/installed.rs` is the one place that
// reads the environment and touches the filesystem.

use std::path::{Path, PathBuf};

/// Every `[Desktop Entry]` field this shell actually reads. Anything else
/// in the file is parsed past and dropped — deliberately: a field this
/// shell has no use for would only be a place for a wrong assumption to
/// hide.
///
/// `icon` is the one field here that nothing RENDERS yet. It is parsed and
/// carried anyway because resolving an `Icon=` NAME against the system icon
/// theme is its own work package, separate from ICON-1 (2026-08-22), which
/// wired up the design boards' own artwork and with it this crate's first
/// `gpui::svg()` plumbing (`crate::icons`). That package needs the real
/// `Icon=` value to have something to resolve against; carrying it now
/// costs nothing and keeps that round a pure rendering change with no
/// re-scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DesktopEntry {
    /// `Type=` — only `Application` is a launchable app (`Link`/`Directory`
    /// are not).
    pub entry_type: String,
    /// Locale-resolved `Name=` (see `parse`'s `locale_prefs` parameter).
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// Verbatim `Exec=`, field codes and all — `exec_to_argv` is what turns
    /// it into something spawnable.
    pub exec: Option<String>,
    /// `TryExec=` — the spec's own "if this binary is missing, do not show
    /// this entry" hint. Honoured in `apps/installed.rs` (it needs `PATH`
    /// and the filesystem; `try_exec_candidates` below is the pure half).
    pub try_exec: Option<String>,
    /// `Icon=` — carried, not rendered. See this struct's own doc comment.
    pub icon: Option<String>,
    pub no_display: bool,
    pub hidden: bool,
    pub terminal: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    pub keywords: Vec<String>,
}

/// A snapshot of the environment variables the XDG directory rules read.
/// Passed in rather than read here so `applications_dirs`/`current_desktops`
/// stay pure (see this file's header comment); `apps/installed.rs::
/// xdg_env_from_process()` is the one place that fills it from `std::env`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct XdgEnv {
    pub home: Option<String>,
    pub data_home: Option<String>,
    pub data_dirs: Option<String>,
    pub current_desktop: Option<String>,
    /// `LC_MESSAGES` / `LC_ALL` / `LANG`, whichever the caller resolved
    /// first — see `locale_prefs`.
    pub lang: Option<String>,
}

/// Parses the `[Desktop Entry]` group of a `.desktop` file.
///
/// `locale_prefs` is an ordered "best first" list of locale names
/// (`["zh_TW", "zh"]`) used to resolve locale-suffixed keys (`Name[zh_TW]`);
/// the unsuffixed key is always the final fallback, so an entry with no
/// translation at all still resolves. Never returns an error: a file this
/// module cannot make sense of yields a `DesktopEntry` whose `entry_type`/
/// `name` are empty, which `should_display` then refuses — a malformed
/// file is skipped, never guessed at.
pub(crate) fn parse(content: &str, locale_prefs: &[String]) -> DesktopEntry {
    // (base key, locale, raw value) triples from the `[Desktop Entry]`
    // group only. Collected first, resolved after, because the best locale
    // match for a key can appear on any line and in any order.
    let mut fields: Vec<(String, Option<String>, String)> = Vec::new();
    let mut in_group = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // A group header. Everything after `[Desktop Entry]` and before
            // the next header belongs to us; a second `[Desktop Entry]`
            // header is malformed but harmless (we simply keep reading).
            in_group = line.trim_end_matches(']').trim_start_matches('[').trim() == "Desktop Entry";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let raw_key = raw_key.trim();
        let (base, locale) = split_locale_suffix(raw_key);
        if base.is_empty() {
            continue;
        }
        fields.push((base.to_string(), locale.map(|l| l.to_string()), raw_value.trim().to_string()));
    }

    // Locale-suffixed keys go through `locale_prefs`; the rest pass `&[]`,
    // which is not an oversight — `Type`/`Exec`/`TryExec`/`NoDisplay`/
    // `Hidden`/`Terminal`/`OnlyShowIn`/`NotShowIn` are not localizable
    // strings in the spec, and honouring a `Type[zh_TW]` would be inventing
    // grammar the format does not have.
    DesktopEntry {
        entry_type: pick(&fields, "Type", &[]).map(|v| unescape(&v)).unwrap_or_default(),
        name: pick(&fields, "Name", locale_prefs).map(|v| unescape(&v)).unwrap_or_default(),
        generic_name: pick(&fields, "GenericName", locale_prefs).map(|v| unescape(&v)).filter(|v| !v.is_empty()),
        comment: pick(&fields, "Comment", locale_prefs).map(|v| unescape(&v)).filter(|v| !v.is_empty()),
        exec: pick(&fields, "Exec", &[]).map(|v| unescape(&v)).filter(|v| !v.is_empty()),
        try_exec: pick(&fields, "TryExec", &[]).map(|v| unescape(&v)).filter(|v| !v.is_empty()),
        icon: pick(&fields, "Icon", locale_prefs).map(|v| unescape(&v)).filter(|v| !v.is_empty()),
        // Only the literal `true` is true, per spec — `1`/`True`/`yes` are
        // not booleans and must not be read as one.
        no_display: pick(&fields, "NoDisplay", &[]).as_deref() == Some("true"),
        hidden: pick(&fields, "Hidden", &[]).as_deref() == Some("true"),
        terminal: pick(&fields, "Terminal", &[]).as_deref() == Some("true"),
        only_show_in: pick(&fields, "OnlyShowIn", &[]).map(|v| split_list(&v)).unwrap_or_default(),
        not_show_in: pick(&fields, "NotShowIn", &[]).map(|v| split_list(&v)).unwrap_or_default(),
        keywords: pick(&fields, "Keywords", locale_prefs).map(|v| split_list(&v)).unwrap_or_default(),
    }
}

/// `Name[zh_TW]` -> `("Name", Some("zh_TW"))`; `Name` -> `("Name", None)`.
fn split_locale_suffix(key: &str) -> (&str, Option<&str>) {
    let Some(open) = key.find('[') else {
        return (key, None);
    };
    if !key.ends_with(']') {
        return (key, None);
    }
    let locale = &key[open + 1..key.len() - 1];
    (key[..open].trim(), Some(locale.trim()))
}

/// Best locale match for `base`, falling back to the unsuffixed key.
/// `locale_prefs` is consulted in order, so `["zh_TW", "zh"]` prefers a
/// Taiwan-specific translation over a generic Chinese one — never the other
/// way round, and never a locale the caller did not ask for.
fn pick(fields: &[(String, Option<String>, String)], base: &str, locale_prefs: &[String]) -> Option<String> {
    for want in locale_prefs {
        if let Some((_, _, value)) = fields.iter().find(|(k, l, _)| k == base && l.as_deref() == Some(want.as_str())) {
            return Some(value.clone());
        }
    }
    fields.iter().find(|(k, l, _)| k == base && l.is_none()).map(|(_, _, v)| v.clone())
}

/// The spec's five escape sequences (§ "Value types"). Anything else after
/// a backslash is left verbatim, backslash included — the spec calls that
/// undefined, and dropping the character would silently corrupt a Windows-
/// style path someone typed into an `Exec` line.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a `;`-terminated string list, honouring the `\;` escape (a
/// semicolon that is PART of a value, not a separator) — the one piece of
/// this grammar a naive `split(';')` gets wrong.
fn split_list(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => current.push(';'),
                Some('s') => current.push(' '),
                Some('n') => current.push('\n'),
                Some('t') => current.push('\t'),
                Some('r') => current.push('\r'),
                Some('\\') => current.push('\\'),
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                }
                None => current.push('\\'),
            },
            ';' => {
                let item = current.trim().to_string();
                if !item.is_empty() {
                    out.push(item);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let item = current.trim().to_string();
    if !item.is_empty() {
        out.push(item);
    }
    out
}

/// Whether this entry should appear in an application list at all.
///
/// Every rule here is the SPEC's, not this shell's invention:
///   * `Type` must be `Application` (`Link`/`Directory` entries are not apps).
///   * `NoDisplay=true` means "this is a handler, not something a person
///     launches" — hidden.
///   * `Hidden=true` means "treat as deleted" — hidden.
///   * `OnlyShowIn` present and not naming the running desktop — hidden.
///   * `NotShowIn` naming the running desktop — hidden.
///   * An entry with no `Name` has nothing to show and no `Exec` has
///     nothing to run.
///
/// `current_desktops` is `XDG_CURRENT_DESKTOP` split on `:` — an EMPTY list
/// (the honest state on a shell that has not claimed a desktop name) makes
/// any `OnlyShowIn` fail to match, which is exactly what the spec says
/// should happen. That deliberately hides e.g. GNOME-only control panels,
/// which is the correct outcome for this appliance rather than a loss.
pub(crate) fn should_display(entry: &DesktopEntry, current_desktops: &[String]) -> bool {
    if entry.entry_type != "Application" {
        return false;
    }
    if entry.no_display || entry.hidden {
        return false;
    }
    if entry.name.trim().is_empty() || entry.exec.is_none() {
        return false;
    }
    // Whole-token equality, never `contains` — "GNOME" must not match
    // "GNOME-Classic" or vice versa (this crate's coding convention 2).
    if !entry.only_show_in.is_empty() && !entry.only_show_in.iter().any(|d| current_desktops.iter().any(|c| c == d)) {
        return false;
    }
    if entry.not_show_in.iter().any(|d| current_desktops.iter().any(|c| c == d)) {
        return false;
    }
    true
}

/// Turns a verbatim `Exec=` value into a spawnable argv, or `None` when
/// there is nothing runnable left. Two jobs the spec makes non-obvious:
///
///   1. **Quoting.** `Exec` is quoted with `"`, and inside quotes a
///      backslash escapes `"`, `` ` ``, `$` and `\` — so a path with a
///      space survives as ONE argument.
///   2. **Field codes.** `%f`/`%u`/`%F`/`%U`/`%i`/`%c`/`%k`/… are
///      placeholders for files/URIs/icon/name that a launcher substitutes.
///      This shell launches apps with no document argument, so every field
///      code is dropped (and `%%` becomes a literal `%`). Leaving them in
///      would pass the literal text `%U` to the program as an argument.
///
/// Returns `None` for an empty result or a first token that looks like a
/// flag — the same "a malformed registry entry must not become an
/// unintended flag" guard `apps.rs::is_safe_cli_arg` applies to the install
/// path, applied here to the launch path.
pub(crate) fn exec_to_argv(exec: &str) -> Option<Vec<String>> {
    let mut argv: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_quotes = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '\\' if in_quotes => match chars.next() {
                Some(escaped @ ('"' | '`' | '$' | '\\')) => current.push(escaped),
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                }
                None => current.push('\\'),
            },
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    argv.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            _ => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        argv.push(current);
    }

    let stripped: Vec<String> = argv.into_iter().map(|token| strip_field_codes(&token)).filter(|token| !token.is_empty()).collect();
    let program = stripped.first()?;
    if program.starts_with('-') {
        return None;
    }
    Some(stripped)
}

/// Every field code the spec defines, plus the deprecated ones still found
/// in the wild (`%d`/`%D`/`%n`/`%N`/`%v`/`%m`). `%%` is the escape for a
/// literal `%` and is the only sequence that leaves anything behind.
fn strip_field_codes(token: &str) -> String {
    const CODES: &[char] = &['f', 'F', 'u', 'U', 'd', 'D', 'n', 'N', 'i', 'c', 'k', 'v', 'm'];
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(code) if CODES.contains(&code) => {}
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out.trim().to_string()
}

/// The desktop-file ID for `file` under `root` — the spec's own rule: the
/// path relative to the applications directory, `/` replaced by `-`, minus
/// the `.desktop` suffix. This is the identity two directories' copies of
/// the same app share (so the higher-precedence directory can win), and,
/// by the same freedesktop convention window managers rely on, the app_id a
/// well-behaved app sets on its Wayland surface.
pub(crate) fn desktop_file_id(root: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(root).ok()?;
    let text = relative.to_str()?;
    let stem = text.strip_suffix(".desktop")?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.replace('/', "-"))
}

/// Where a `TryExec=` value could live. An absolute or relative path (it
/// contains a `/`) is taken as-is; a bare program name is looked for in
/// each `PATH` directory. `apps/installed.rs` is what actually checks
/// whether any candidate exists — keeping the filesystem out of here is
/// what makes this testable.
pub(crate) fn try_exec_candidates(try_exec: &str, path_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let value = try_exec.trim();
    if value.is_empty() {
        return Vec::new();
    }
    if value.contains('/') {
        return vec![PathBuf::from(value)];
    }
    path_dirs.iter().map(|dir| dir.join(value)).collect()
}

/// The XDG applications directories, highest precedence first.
///
/// Standard part (XDG Base Directory Specification): `$XDG_DATA_HOME/
/// applications` (default `$HOME/.local/share/applications`), then each
/// entry of `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`) with
/// `/applications` appended.
///
/// Then, appended at LOWER precedence, flatpak's own export directories.
/// This is not decoration: flatpak publishes each installed app's real
/// `.desktop` file (with its localized `Name=` and its `Icon=`) into
/// `<installation>/exports/share/applications`, and it is flatpak's
/// `profile.d` snippet — not the base spec — that puts those on
/// `XDG_DATA_DIRS`. A session that did not source that snippet (a kiosk
/// `cage` unit, this shell launched straight from a systemd service) would
/// otherwise see flatpak apps with no icon and no translated name at all.
/// The appliance's own extra installation (`/data/flatpak`, declared by
/// `/etc/flatpak/installations.d/10-duduclaw-data.conf`) is listed FIRST
/// among those, since that is where this appliance actually installs to.
/// Duplicates against the standard part are removed, so a session that DID
/// source the snippet is unaffected.
pub(crate) fn applications_dirs(env: &XdgEnv) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let data_home = resolved_data_home(env);
    if let Some(home) = &data_home {
        dirs.push(PathBuf::from(home).join("applications"));
    }
    let data_dirs = env.data_dirs.as_deref().filter(|v| !v.trim().is_empty()).unwrap_or("/usr/local/share:/usr/share");
    for part in data_dirs.split(':') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        dirs.push(PathBuf::from(part).join("applications"));
    }
    dirs.push(PathBuf::from("/data/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Some(home) = &data_home {
        dirs.push(PathBuf::from(home).join("flatpak/exports/share/applications"));
    }
    dirs.dedup();
    let mut seen: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !seen.contains(&dir) {
            seen.push(dir);
        }
    }
    seen
}

fn resolved_data_home(env: &XdgEnv) -> Option<String> {
    if let Some(explicit) = env.data_home.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(explicit.to_string());
    }
    let home = env.home.as_deref().map(str::trim).filter(|v| !v.is_empty())?;
    Some(format!("{home}/.local/share"))
}

/// `XDG_CURRENT_DESKTOP` split on `:`. An unset/empty variable yields an
/// EMPTY list, which `should_display` treats exactly as the spec says —
/// see that fn's own doc comment. Deliberately not defaulted to a made-up
/// desktop name.
pub(crate) fn current_desktops(env: &XdgEnv) -> Vec<String> {
    env.current_desktop
        .as_deref()
        .unwrap_or("")
        .split(':')
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

/// Ordered locale preference list for locale-suffixed keys, derived from a
/// POSIX locale name (`zh_TW.UTF-8@modifier` -> `["zh_TW", "zh"]`).
///
/// `None`, an empty value, or the `C`/`POSIX` locales yield the SAME
/// default this shell's own UI already hardcodes everywhere else
/// (`i18n::Locale::ZhTw`, see every `t(Locale::ZhTw, ...)` call site) —
/// consistency with what the operator is actually reading on screen, not a
/// second, invisible locale policy.
pub(crate) fn locale_prefs(lang: Option<&str>) -> Vec<String> {
    let raw = lang.unwrap_or("").trim();
    let base = raw.split(['.', '@']).next().unwrap_or("").trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return vec!["zh_TW".to_string(), "zh".to_string()];
    }
    let mut prefs = vec![base.to_string()];
    if let Some((language, _)) = base.split_once('_') {
        if !language.is_empty() && language != base {
            prefs.push(language.to_string());
        }
    }
    prefs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zh_prefs() -> Vec<String> {
        vec!["zh_TW".to_string(), "zh".to_string()]
    }

    const CHROMIUM: &str = "\
[Desktop Entry]
Version=1.0
Name=Chromium Web Browser
Name[zh_TW]=Chromium 網頁瀏覽器
Name[ja]=Chromium ウェブブラウザ
GenericName=Web Browser
Comment=Access the Internet
Exec=/usr/bin/chromium %U
TryExec=/usr/bin/chromium
Icon=chromium
Terminal=false
Type=Application
Categories=Network;WebBrowser;
Keywords=web;browser;internet;

[Desktop Action new-window]
Name=New Window
Exec=/usr/bin/chromium
";

    #[test]
    fn parses_the_fields_the_shell_actually_uses() {
        let entry = parse(CHROMIUM, &zh_prefs());
        assert_eq!(entry.entry_type, "Application");
        assert_eq!(entry.name, "Chromium 網頁瀏覽器", "a zh_TW translation must beat the untranslated Name");
        assert_eq!(entry.generic_name.as_deref(), Some("Web Browser"));
        assert_eq!(entry.exec.as_deref(), Some("/usr/bin/chromium %U"));
        assert_eq!(entry.try_exec.as_deref(), Some("/usr/bin/chromium"));
        assert_eq!(entry.icon.as_deref(), Some("chromium"), "Icon= must be preserved for the icon work package");
        assert!(!entry.terminal);
        assert_eq!(entry.keywords, vec!["web", "browser", "internet"]);
    }

    #[test]
    fn keys_from_other_groups_are_never_read() {
        // `[Desktop Action new-window]` also has a `Name`/`Exec`. Reading
        // it would rename the app to "New Window".
        let entry = parse(CHROMIUM, &[]);
        assert_eq!(entry.name, "Chromium Web Browser");
        assert_eq!(entry.exec.as_deref(), Some("/usr/bin/chromium %U"));
    }

    #[test]
    fn locale_preference_is_ordered_and_falls_back_to_the_plain_key() {
        assert_eq!(parse(CHROMIUM, &["ja".to_string()]).name, "Chromium ウェブブラウザ");
        assert_eq!(parse(CHROMIUM, &["de".to_string()]).name, "Chromium Web Browser", "a locale with no entry falls back to the plain key, never to some other locale");
        // The WHOLE preference list is consulted in order before the plain
        // key: this file has no `Name[zh]`, so the second preference wins.
        assert_eq!(parse(CHROMIUM, &["zh".to_string(), "zh_TW".to_string()]).name, "Chromium 網頁瀏覽器");
        // …and order decides between two that BOTH exist.
        assert_eq!(parse(CHROMIUM, &["ja".to_string(), "zh_TW".to_string()]).name, "Chromium ウェブブラウザ");
        assert_eq!(parse(CHROMIUM, &["zh_TW".to_string(), "ja".to_string()]).name, "Chromium 網頁瀏覽器");
    }

    #[test]
    fn comments_blank_lines_and_junk_lines_are_skipped_not_fatal() {
        let entry = parse("# a comment\n\n[Desktop Entry]\n# another\nnot-a-key-value-line\nType=Application\nName=Ok\nExec=ok\n", &[]);
        assert_eq!(entry.name, "Ok");
        assert_eq!(entry.entry_type, "Application");
    }

    #[test]
    fn a_file_with_no_desktop_entry_group_yields_an_undisplayable_entry() {
        let entry = parse("[Something Else]\nName=Nope\nType=Application\nExec=nope\n", &[]);
        assert_eq!(entry, DesktopEntry::default());
        assert!(!should_display(&entry, &[]));
    }

    #[test]
    fn escape_sequences_are_decoded() {
        let entry = parse("[Desktop Entry]\nType=Application\nName=A\\sB\\nC\\\\D\nExec=x\n", &[]);
        assert_eq!(entry.name, "A B\nC\\D");
    }

    #[test]
    fn an_unknown_escape_keeps_the_backslash_rather_than_silently_eating_a_character() {
        let entry = parse("[Desktop Entry]\nType=Application\nName=C:\\\\x\\q\nExec=x\n", &[]);
        assert_eq!(entry.name, "C:\\x\\q");
    }

    #[test]
    fn semicolon_lists_honour_the_escape() {
        let entry = parse("[Desktop Entry]\nType=Application\nName=n\nExec=x\nKeywords=a;b\\;c;d;\n", &[]);
        assert_eq!(entry.keywords, vec!["a", "b;c", "d"], "an escaped semicolon is part of the value, not a separator");
    }

    // ── should_display ──────────────────────────────────────────────────

    fn app(extra: &str) -> DesktopEntry {
        parse(&format!("[Desktop Entry]\nType=Application\nName=App\nExec=app\n{extra}"), &[])
    }

    #[test]
    fn a_plain_application_is_displayed() {
        assert!(should_display(&app(""), &[]));
    }

    #[test]
    fn nodisplay_and_hidden_are_both_honoured() {
        assert!(!should_display(&app("NoDisplay=true\n"), &[]));
        assert!(!should_display(&app("Hidden=true\n"), &[]));
        // Only the literal `true` is true, per spec — `1`/`True` are not.
        assert!(should_display(&app("NoDisplay=1\n"), &[]));
        assert!(should_display(&app("Hidden=True\n"), &[]));
    }

    #[test]
    fn non_application_types_are_not_apps() {
        assert!(!should_display(&parse("[Desktop Entry]\nType=Link\nName=L\nURL=http://x\n", &[]), &[]));
        assert!(!should_display(&parse("[Desktop Entry]\nType=Directory\nName=D\n", &[]), &[]));
    }

    #[test]
    fn an_entry_with_no_name_or_no_exec_is_refused() {
        assert!(!should_display(&parse("[Desktop Entry]\nType=Application\nExec=x\n", &[]), &[]));
        assert!(!should_display(&parse("[Desktop Entry]\nType=Application\nName=n\n", &[]), &[]));
    }

    #[test]
    fn only_show_in_and_not_show_in_match_whole_tokens() {
        let gnome_only = app("OnlyShowIn=GNOME;\n");
        assert!(!should_display(&gnome_only, &[]), "no declared desktop must not satisfy OnlyShowIn");
        assert!(!should_display(&gnome_only, &["GNOME-Classic".to_string()]), "substring must never count as a match");
        assert!(should_display(&gnome_only, &["KDE".to_string(), "GNOME".to_string()]));

        let not_gnome = app("NotShowIn=GNOME;KDE;\n");
        assert!(should_display(&not_gnome, &["DuDuClaw".to_string()]));
        assert!(!should_display(&not_gnome, &["KDE".to_string()]));
    }

    // ── exec_to_argv ────────────────────────────────────────────────────

    #[test]
    fn field_codes_are_stripped_so_nothing_receives_a_literal_percent_u() {
        assert_eq!(exec_to_argv("/usr/bin/chromium %U"), Some(vec!["/usr/bin/chromium".to_string()]));
        assert_eq!(exec_to_argv("app %f %F %u %i %c %k"), Some(vec!["app".to_string()]));
        assert_eq!(exec_to_argv("app --file=%f --keep"), Some(vec!["app".to_string(), "--file=".to_string(), "--keep".to_string()]));
    }

    #[test]
    fn a_double_percent_is_a_literal_percent() {
        assert_eq!(exec_to_argv("app 100%%"), Some(vec!["app".to_string(), "100%".to_string()]));
    }

    #[test]
    fn quoted_arguments_survive_as_one_token() {
        assert_eq!(
            exec_to_argv("\"/opt/My App/bin/run\" --flag \"a b\""),
            Some(vec!["/opt/My App/bin/run".to_string(), "--flag".to_string(), "a b".to_string()])
        );
        assert_eq!(exec_to_argv("app \"say \\\"hi\\\"\""), Some(vec!["app".to_string(), "say \"hi\"".to_string()]));
    }

    #[test]
    fn the_real_flatpak_exported_exec_line_parses() {
        // Verbatim shape flatpak writes into an exported `.desktop` file.
        assert_eq!(
            exec_to_argv("/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=chromium org.chromium.Chromium @@u %U @@"),
            Some(vec![
                "/usr/bin/flatpak".to_string(),
                "run".to_string(),
                "--branch=stable".to_string(),
                "--arch=x86_64".to_string(),
                "--command=chromium".to_string(),
                "org.chromium.Chromium".to_string(),
                "@@u".to_string(),
                "@@".to_string(),
            ])
        );
    }

    #[test]
    fn an_exec_that_would_run_nothing_or_a_flag_is_refused() {
        assert_eq!(exec_to_argv(""), None);
        assert_eq!(exec_to_argv("   "), None);
        assert_eq!(exec_to_argv("%U"), None, "field codes only leaves nothing to run");
        assert_eq!(exec_to_argv("--rm -rf /"), None, "a flag-shaped program name is never spawned");
    }

    // ── ids / paths / env ───────────────────────────────────────────────

    #[test]
    fn desktop_file_id_follows_the_spec_rule() {
        let root = Path::new("/usr/share/applications");
        assert_eq!(desktop_file_id(root, Path::new("/usr/share/applications/firefox.desktop")).as_deref(), Some("firefox"));
        assert_eq!(desktop_file_id(root, Path::new("/usr/share/applications/kde4/kate.desktop")).as_deref(), Some("kde4-kate"));
        assert_eq!(desktop_file_id(root, Path::new("/usr/share/applications/notes.txt")), None);
        assert_eq!(desktop_file_id(root, Path::new("/elsewhere/x.desktop")), None);
    }

    #[test]
    fn try_exec_candidates_split_path_lookup_from_an_explicit_path() {
        let path_dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        assert_eq!(try_exec_candidates("/opt/x/bin/x", &path_dirs), vec![PathBuf::from("/opt/x/bin/x")]);
        assert_eq!(try_exec_candidates("chromium", &path_dirs), vec![PathBuf::from("/usr/bin/chromium"), PathBuf::from("/bin/chromium")]);
        assert!(try_exec_candidates("  ", &path_dirs).is_empty());
    }

    #[test]
    fn applications_dirs_follow_xdg_precedence_and_append_the_flatpak_exports() {
        let env = XdgEnv { home: Some("/home/op".to_string()), ..Default::default() };
        assert_eq!(
            applications_dirs(&env),
            vec![
                PathBuf::from("/home/op/.local/share/applications"),
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
                PathBuf::from("/data/flatpak/exports/share/applications"),
                PathBuf::from("/var/lib/flatpak/exports/share/applications"),
                PathBuf::from("/home/op/.local/share/flatpak/exports/share/applications"),
            ]
        );
    }

    #[test]
    fn an_explicit_xdg_data_home_and_data_dirs_are_honoured_and_never_duplicated() {
        let env = XdgEnv {
            home: Some("/home/op".to_string()),
            data_home: Some("/custom/data".to_string()),
            // Deliberately repeats a directory the flatpak-export block also
            // adds: a session that DID source flatpak's profile.d snippet
            // must not get it twice.
            data_dirs: Some("/opt/share:/var/lib/flatpak/exports/share:/usr/share".to_string()),
            ..Default::default()
        };
        assert_eq!(
            applications_dirs(&env),
            vec![
                PathBuf::from("/custom/data/applications"),
                PathBuf::from("/opt/share/applications"),
                PathBuf::from("/var/lib/flatpak/exports/share/applications"),
                PathBuf::from("/usr/share/applications"),
                PathBuf::from("/data/flatpak/exports/share/applications"),
                PathBuf::from("/custom/data/flatpak/exports/share/applications"),
            ]
        );
    }

    #[test]
    fn no_home_at_all_still_yields_the_system_directories() {
        let dirs = applications_dirs(&XdgEnv::default());
        assert!(dirs.contains(&PathBuf::from("/usr/share/applications")));
        assert!(dirs.iter().all(|d| !d.starts_with("/.local")), "an empty HOME must never build a `/.local/...` path");
    }

    #[test]
    fn current_desktops_splits_on_colon_and_is_empty_when_unset() {
        assert!(current_desktops(&XdgEnv::default()).is_empty());
        let env = XdgEnv { current_desktop: Some("DuDuClaw:wlroots".to_string()), ..Default::default() };
        assert_eq!(current_desktops(&env), vec!["DuDuClaw", "wlroots"]);
    }

    #[test]
    fn locale_prefs_derive_language_fallback_and_default_to_the_shells_own_locale() {
        assert_eq!(locale_prefs(Some("zh_TW.UTF-8")), vec!["zh_TW", "zh"]);
        assert_eq!(locale_prefs(Some("ja_JP.UTF-8@modifier")), vec!["ja_JP", "ja"]);
        assert_eq!(locale_prefs(Some("en")), vec!["en"]);
        assert_eq!(locale_prefs(None), vec!["zh_TW", "zh"]);
        assert_eq!(locale_prefs(Some("C")), vec!["zh_TW", "zh"]);
        assert_eq!(locale_prefs(Some("POSIX")), vec!["zh_TW", "zh"]);
    }
}
