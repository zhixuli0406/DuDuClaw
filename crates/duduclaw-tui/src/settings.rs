//! Local presentation settings, never authority or credentials.
use crate::{Error, Result, state::POLL_INTERVAL, strings};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Glyphs {
    #[default]
    Ascii,
    Unicode,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AmbiguousWidth {
    #[default]
    Narrow,
    Wide,
}
#[derive(Clone, Debug)]
pub struct Settings {
    pub glyphs: Glyphs,
    pub ambiguous_width: AmbiguousWidth,
    pub poll_interval: Duration,
    pub notifications: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            glyphs: Glyphs::Ascii,
            ambiguous_width: AmbiguousWidth::Narrow,
            poll_interval: POLL_INTERVAL,
            notifications: false,
        }
    }
}
pub const MIN_POLL_SECONDS: u64 = 5;
pub const MAX_POLL_SECONDS: u64 = 3600;
const MAX_FILE_BYTES: u64 = 64 * 1024;
impl Settings {
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
        let mut result = Self::default();
        let mut bad = false;
        for (key, value) in table {
            match key.as_str() {
                "notifications" => match value.as_bool() {
                    Some(enabled) => result.notifications = enabled,
                    None => bad = true,
                },
                "glyphs" => match value.as_str() {
                    Some("ascii") => {}
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
        (result, bad.then_some(strings::SETTINGS_NOTICE))
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
        assert_eq!(settings.glyphs, Glyphs::Ascii);
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
