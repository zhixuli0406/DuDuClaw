//! Local presentation settings, never authority or credentials.
use crate::{Error, Result, state::POLL_INTERVAL, strings};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyphs {
    Ascii,
    Unicode,
}
impl Glyphs {
    pub fn detect(lc_all: Option<&str>, lc_ctype: Option<&str>, lang: Option<&str>, term: Option<&str>) -> Self {
        if matches!(term, Some("linux" | "dumb")) { return Self::Ascii; }
        let locale = [lc_all, lc_ctype, lang].into_iter().flatten().find(|s| !s.is_empty()).unwrap_or("").to_ascii_lowercase();
        if locale.contains("utf-8") || locale.contains("utf8") { Self::Unicode } else { Self::Ascii }
    }
}
impl Default for Glyphs {
    fn default() -> Self {
        Self::detect(std::env::var("LC_ALL").ok().as_deref(), std::env::var("LC_CTYPE").ok().as_deref(), std::env::var("LANG").ok().as_deref(), std::env::var("TERM").ok().as_deref())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AmbiguousWidth {
    #[default]
    Narrow,
    Wide,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DefaultLayout {
    #[default]
    Single,
    Vertical,
    Horizontal,
}
impl DefaultLayout {
    pub fn blocks(self) -> crate::blocks::Blocks {
        let mut blocks = crate::blocks::Blocks::default();
        match self {
            Self::Single => {},
            Self::Vertical => blocks.add(crate::blocks::Split::Vertical),
            Self::Horizontal => blocks.add(crate::blocks::Split::Horizontal),
        }
        blocks.select(0);
        blocks
    }
}
#[derive(Clone, Debug)]
pub struct Settings {
    pub theme: crate::theme::ThemeMode,
    pub colors: crate::theme::Theme,
    pub glyphs: Glyphs,
    pub ambiguous_width: AmbiguousWidth,
    pub poll_interval: Duration,
    pub notifications: bool,
    pub default_layout: DefaultLayout,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: crate::theme::ThemeMode::Auto,
            colors: crate::theme::Theme::new(crate::theme::ThemeMode::Dark, crate::theme::Capability::from_env()),
            glyphs: Glyphs::default(),
            ambiguous_width: AmbiguousWidth::Narrow,
            poll_interval: POLL_INTERVAL,
            notifications: false,
            default_layout: DefaultLayout::Single,
        }
    }
}
pub const MIN_POLL_SECONDS: u64 = 5;
pub const MAX_POLL_SECONDS: u64 = 3600;
const MAX_FILE_BYTES: u64 = 64 * 1024;
impl Settings {
    pub fn resolve_theme(&mut self, background: Option<(u8, u8, u8)>) {
        let mode = crate::theme::resolve(self.theme, background, std::env::var("COLORFGBG").ok().as_deref());
        self.colors = crate::theme::Theme::new(mode, self.colors.capability);
    }
    #[cfg(test)]
    pub(crate) fn test_defaults() -> Self {
        Self { glyphs: Glyphs::Ascii, colors: crate::theme::Theme::new(crate::theme::ThemeMode::Dark, crate::theme::Capability::TrueColor), ..Self::default() }
    }

    pub fn load(home: &Path) -> (Self, Option<&'static str>) {
        let file = match crate::local_file::open_regular(&home.join("tui.toml")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(_) => return (Self::default(), Some(strings::SETTINGS_NOTICE)),
        };
        let mut text = String::new();
        if file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).is_ok()
            && text.len() as u64 <= MAX_FILE_BYTES
        {
            Self::parse(&text)
        } else {
            (Self::default(), Some(strings::SETTINGS_NOTICE))
        }
    }
    pub fn parse(text: &str) -> (Self, Option<&'static str>) {
        let Ok(table) = text.parse::<toml::Table>() else {
            return (Self::default(), Some(strings::SETTINGS_NOTICE));
        };
        let removed_command = table.get("pty").and_then(toml::Value::as_table)
            .is_some_and(|pty| pty.contains_key("command"));
        let mut result = Self::default();
        let mut bad = false;
        for (key, value) in table {
            match key.as_str() {
                "default_layout" => match value.as_str() {
                    Some("single") => {},
                    Some("vertical") => result.default_layout = DefaultLayout::Vertical,
                    Some("horizontal") => result.default_layout = DefaultLayout::Horizontal,
                    _ => bad = true,
                },
                "notifications" => match value.as_bool() {
                    Some(enabled) => result.notifications = enabled,
                    None => bad = true,
                },
                "theme" => match value.as_str() {
                    Some("auto") => result.theme = crate::theme::ThemeMode::Auto,
                    Some("light") => result.theme = crate::theme::ThemeMode::Light,
                    Some("dark") => result.theme = crate::theme::ThemeMode::Dark,
                    _ => bad = true,
                },
                "glyphs" => match value.as_str() {
                    Some("ascii") => result.glyphs = Glyphs::Ascii,
                    Some("unicode") => result.glyphs = Glyphs::Unicode,
                    _ => bad = true,
                },
                "ambiguous_width" => match value.as_str() {
                    Some("narrow") => {}
                    Some("wide") => result.ambiguous_width = AmbiguousWidth::Wide,
                    _ => bad = true,
                },
                "poll_interval_seconds" => match value.as_integer() {
                    Some(n) if (MIN_POLL_SECONDS as i64..=MAX_POLL_SECONDS as i64).contains(&n) => {
                        result.poll_interval = Duration::from_secs(n as u64)
                    }
                    _ => bad = true,
                },
                _ => bad = true,
            }
        }
        result.resolve_theme(None);
        (result, if removed_command { Some(strings::PTY_COMMAND_IGNORED) } else { bad.then_some(strings::SETTINGS_NOTICE) })
    }
}
/// An explicit CLI home wins; otherwise honor DUDUCLAW_HOME before the OS home.
pub fn resolve_home(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| {
            std::env::var_os("DUDUCLAW_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|p| PathBuf::from(p).join(".duduclaw"))
        })
        .ok_or(Error::LocalState)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications_require_explicit_boolean_opt_in() {
        // Default false also covers SSH_CONNECTION; no environment auto-enable.
        assert!(!Settings::default().notifications);
        assert!(!Settings::parse("").0.notifications);
        assert!(Settings::parse("notifications = true").0.notifications);
        let (settings, notice) = Settings::parse("notifications = 'true'");
        assert!(!settings.notifications);
        assert!(notice.is_some());
    }
    #[test]
    fn oversized_settings_fall_back() {
        let home = tempfile::tempdir().unwrap();
        let mut text = String::from("glyphs = 'unicode'\n#");
        text.extend(std::iter::repeat_n('x', MAX_FILE_BYTES as usize - text.len()));
        std::fs::write(home.path().join("tui.toml"), &text).unwrap();
        let (settings, notice) = Settings::load(home.path());
        assert_eq!(settings.glyphs, Glyphs::Unicode);
        assert!(notice.is_none());
        text.push('x');
        std::fs::write(home.path().join("tui.toml"), text).unwrap();
        let (settings, notice) = Settings::load(home.path());
        assert_eq!(settings.glyphs, Glyphs::default());
        assert!(notice.is_some());
    }
    #[test]
    fn bad_fields_fall_back_individually() {
        let (s, n) = Settings::parse(
            "glyphs = 'unicode'\npoll_interval_seconds = 0\nambiguous_width = 'wide'",
        );
        assert_eq!(s.glyphs, Glyphs::Unicode);
        assert_eq!(s.ambiguous_width, AmbiguousWidth::Wide);
        assert_eq!(s.poll_interval, POLL_INTERVAL);
        assert!(n.is_some());
        for t in [
            "broken[",
            "glyphs = 3",
            "poll_interval_seconds = -5",
            "poll_interval_seconds = 3601",
            "unknown = true",
        ] {
            assert!(Settings::parse(t).1.is_some());
        }
        assert!(Settings::parse("poll_interval_seconds = 5").1.is_none());
    }
}

#[cfg(test)]
mod m3_tests {
    use super::*;
    #[test]
    fn every_key_validates_independently_and_cannot_change_security() {
        let (s, notice) = Settings::parse("glyphs = 'unicode'\nnotifications = true\npoll_interval_seconds = 3600\ndefault_layout = 'horizontal'\nambiguous_width = 'wide'");
        assert!(notice.is_none());
        assert_eq!(s.default_layout, DefaultLayout::Horizontal);
        assert_eq!(s.default_layout.blocks().panes.len(), 2);
        assert_eq!(s.default_layout.blocks().split, crate::blocks::Split::Horizontal);
        assert_eq!(s.poll_interval.as_secs(), 3600);
        assert!(s.notifications);
        for text in ["default_layout = 'grid'", "default_layout = 2", "default_layout = ['single']", "notifications = 1", "ambiguous_width = false", "glyphs = 'ASCII'", "poll_interval_seconds = 5.0", "url = 'ws://evil'", "token = 'secret'", "[auth]\nallow = true"] {
            let (s, notice) = Settings::parse(text);
            assert!(notice.is_some(), "{text}");
            assert_eq!(s.default_layout, DefaultLayout::Single);
            assert_eq!(s.glyphs, Glyphs::default());
            assert!(!s.notifications);
        }
        let (s, n) = Settings::parse("default_layout = 'vertical'\npoll_interval_seconds = -1\nnotifications = 'true'");
        assert!(n.is_some()); assert_eq!(s.default_layout, DefaultLayout::Vertical);
        assert_eq!(s.poll_interval, POLL_INTERVAL); assert!(!s.notifications);
    }
}

#[cfg(test)]
mod visual_tests {
    use super::*;
    #[test]
    fn glyph_detection_matrix_and_explicit_override() {
        for term in [Some("xterm-256color"), Some("screen"), None] {
            for locale in ["zh_TW.UTF-8", "C.utf8", "ja_JP.utf-8"] {
                assert_eq!(Glyphs::detect(None,None,Some(locale),term), Glyphs::Unicode);
                assert_eq!(Glyphs::detect(Some("C"),None,Some(locale),term), Glyphs::Ascii);
                assert_eq!(Glyphs::detect(Some(""),Some(locale),Some("C"),term), Glyphs::Unicode);
                assert_eq!(Glyphs::detect(None,Some("C"),Some(locale),term), Glyphs::Ascii);
            }
        }
        for term in [Some("linux"),Some("dumb")] {
            assert_eq!(Glyphs::detect(Some("C.UTF-8"),None,None,term), Glyphs::Ascii);
        }
        assert_eq!(Glyphs::detect(None,None,None,None), Glyphs::Ascii);
        assert_eq!(Settings::parse("glyphs = 'ascii'").0.glyphs, Glyphs::Ascii);
        assert_eq!(Settings::parse("glyphs = 'unicode'").0.glyphs, Glyphs::Unicode);
    }
    #[test]
    fn theme_is_strict_and_bad_fields_keep_valid_fields() {
        use crate::theme::ThemeMode;
        for (value, expected) in [("auto",ThemeMode::Auto),("light",ThemeMode::Light),("dark",ThemeMode::Dark)] {
            let (s, notice) = Settings::parse(&format!("theme = '{value}'"));
            assert_eq!(s.theme, expected); assert!(notice.is_none());
        }
        for bad in ["'LIGHT'", "true", "4", "['light']"] {
            let (s, notice) = Settings::parse(&format!("theme = {bad}\nglyphs = 'ascii'"));
            assert_eq!(s.theme, ThemeMode::Auto); assert_eq!(s.glyphs, Glyphs::Ascii); assert!(notice.is_some());
        }
        let (s, notice) = Settings::parse("theme = 'light'\nunknown = true");
        assert_eq!(s.theme, ThemeMode::Light); assert!(notice.is_some());
    }
}
