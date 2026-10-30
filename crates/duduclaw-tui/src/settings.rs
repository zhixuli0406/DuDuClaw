//! Local presentation settings, never authority or credentials.
use crate::{Error, Result, state::POLL_INTERVAL, strings};
use std::{
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
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            glyphs: Glyphs::Ascii,
            ambiguous_width: AmbiguousWidth::Narrow,
            poll_interval: POLL_INTERVAL,
        }
    }
}
pub const MIN_POLL_SECONDS: u64 = 5;
pub const MAX_POLL_SECONDS: u64 = 3600;
impl Settings {
    pub fn load(home: &Path) -> (Self, Option<&'static str>) {
        match std::fs::read_to_string(home.join("tui.toml")) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(_) => (Self::default(), Some(strings::SETTINGS_NOTICE)),
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
