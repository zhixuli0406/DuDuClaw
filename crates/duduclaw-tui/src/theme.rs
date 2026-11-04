//! Semantic presentation tokens. No gateway text or terminal replies are emitted here.
use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    Auto,
    Light,
    Dark,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability { TrueColor, Ansi256, Ansi16, NoColor }
impl Capability {
    pub fn detect(no_color: bool, colorterm: Option<&str>, term: Option<&str>) -> Self {
        if no_color { Self::NoColor }
        else if colorterm.is_some_and(|s| s.eq_ignore_ascii_case("truecolor") || s.eq_ignore_ascii_case("24bit")) { Self::TrueColor }
        else if term.is_some_and(|s| s.contains("256color")) { Self::Ansi256 }
        else { Self::Ansi16 }
    }
    pub fn from_env() -> Self {
        Self::detect(std::env::var_os("NO_COLOR").is_some(), std::env::var("COLORTERM").ok().as_deref(), std::env::var("TERM").ok().as_deref())
    }
}

/// Explicit preference, then OSC 11, then COLORFGBG, finally dark.
pub fn resolve(mode: ThemeMode, background: Option<(u8, u8, u8)>, colorfgbg: Option<&str>) -> ThemeMode {
    if mode != ThemeMode::Auto { return mode; }
    if let Some(rgb) = background { return if luminance(rgb) > 0.179 { ThemeMode::Light } else { ThemeMode::Dark }; }
    if let Some(index) = colorfgbg.filter(|s| s.len() <= 64)
        .and_then(|s| s.rsplit(';').next()).and_then(|s| s.parse::<u8>().ok()) {
        return if luminance(xterm_rgb(index)) > 0.179 { ThemeMode::Light } else { ThemeMode::Dark };
    }
    ThemeMode::Dark
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub capability: Capability,
    pub background: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub border_focus: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub header_bg: Color,
}
impl Theme {
    pub fn new(mode: ThemeMode, capability: Capability) -> Self {
        let light = mode == ThemeMode::Light;
        if capability == Capability::Ansi16 {
            let (background, text, muted, accent, success, danger, info) = if light {
                (Color::White, Color::Black, Color::Black, Color::Red, Color::Green, Color::Magenta, Color::Blue)
            } else {
                (Color::Black, Color::White, Color::Gray, Color::LightYellow, Color::LightGreen, Color::LightRed, Color::LightCyan)
            };
            return Self { capability, background, text, muted, border: muted, border_focus: accent,
                accent, success, warning: accent, danger, info, selection_fg: text,
                selection_bg: background, header_bg: background };
        }
        let (bg, text, muted, accent, success, danger, info) = if light {
            (0xF5F4F0, 0x102B48, 0x526274, 0x8A5A10, 0x146C4A, 0xA52E4D, 0x235F98)
        } else {
            (0x102B48, 0xF5F4F0, 0xA9B6C4, 0xE0A54F, 0x6AD6AA, 0xFF92A5, 0x8CC8FF)
        };
        let map = |hex| degrade(rgb(hex), capability);
        Self {
            capability, background: map(bg), text: map(text), muted: map(muted),
            border: map(muted), border_focus: map(accent), accent: map(accent),
            success: map(success), warning: map(accent), danger: map(danger), info: map(info),
            selection_fg: map(text), selection_bg: map(if capability == Capability::Ansi256 { bg } else if light { 0xE5E1D7 } else { 0x1B3B58 }),
            header_bg: map(bg),
        }
    }
    pub fn base(self) -> Style { Style::default().fg(self.text).bg(self.background) }
    pub fn muted(self) -> Style { self.base().fg(self.muted) }
    pub fn emphasis(self, color: Color) -> Style { self.base().fg(color).add_modifier(Modifier::BOLD) }
    pub fn selected(self) -> Style {
        if self.capability == Capability::NoColor {
            self.base().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else { self.base().fg(self.selection_fg).bg(self.selection_bg).add_modifier(Modifier::BOLD) }
    }
    pub fn border_style(self, focused: bool) -> Style {
        if focused { self.emphasis(self.border_focus) } else { self.base().fg(self.border) }
    }
}
fn rgb(hex: u32) -> (u8, u8, u8) { ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8) }
const ANSI: [(u8,u8,u8); 16] = [
    (0,0,0), (128,0,0), (0,128,0), (128,128,0), (0,0,128), (128,0,128), (0,128,128), (192,192,192),
    (128,128,128), (255,0,0), (0,255,0), (255,255,0), (0,0,255), (255,0,255), (0,255,255), (255,255,255),
];
fn xterm_rgb(index: u8) -> (u8,u8,u8) {
    if index < 16 { return ANSI[usize::from(index)]; }
    if index >= 232 { let n = 8 + (index - 232) * 10; return (n,n,n); }
    let n = index - 16;
    let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
    (level(n / 36), level(n / 6 % 6), level(n % 6))
}
fn distance(a: (u8,u8,u8), b: (u8,u8,u8)) -> u32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2) as u32;
    d(a.0,b.0) + d(a.1,b.1) + d(a.2,b.2)
}
pub fn degrade(value: (u8,u8,u8), capability: Capability) -> Color {
    match capability {
        Capability::NoColor => Color::Reset,
        Capability::TrueColor => Color::Rgb(value.0, value.1, value.2),
        // The first 16 xterm slots are user-configurable; use the fixed cube/greys.
        Capability::Ansi256 => Color::Indexed((16..=255).min_by_key(|&i| distance(value, xterm_rgb(i))).unwrap()),
        Capability::Ansi16 => {
            let index = (0..16).min_by_key(|&i| distance(value, ANSI[i])).unwrap();
            [Color::Black, Color::Red, Color::Green, Color::Yellow, Color::Blue, Color::Magenta,
                Color::Cyan, Color::Gray, Color::DarkGray, Color::LightRed, Color::LightGreen,
                Color::LightYellow, Color::LightBlue, Color::LightMagenta, Color::LightCyan, Color::White][index]
        }
    }
}
/// Adapt embedded terminal colors too; NO_COLOR covers every painted cell.
pub fn adapt(color: Color, capability: Capability) -> Color {
    if capability == Capability::NoColor { return Color::Reset; }
    match color {
        Color::Rgb(r,g,b) => degrade((r,g,b), capability),
        Color::Indexed(i) if capability == Capability::Ansi16 => degrade(xterm_rgb(i), capability),
        _ => color,
    }
}
fn luminance((r,g,b): (u8,u8,u8)) -> f64 {
    let linear = |v: u8| { let s = f64::from(v) / 255.0; if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) } };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_order_and_bounded_environment() {
        assert_eq!(resolve(ThemeMode::Light, Some((0,0,0)), Some("15;0")), ThemeMode::Light);
        assert_eq!(resolve(ThemeMode::Dark, Some((255,255,255)), Some("0;15")), ThemeMode::Dark);
        assert_eq!(resolve(ThemeMode::Auto, Some((255,255,255)), Some("15;0")), ThemeMode::Light);
        assert_eq!(resolve(ThemeMode::Auto, Some((0,0,0)), Some("0;15")), ThemeMode::Dark);
        assert_eq!(resolve(ThemeMode::Auto, None, Some("0;15")), ThemeMode::Light);
        assert_eq!(resolve(ThemeMode::Auto, None, Some("15;0")), ThemeMode::Dark);
        for value in [None, Some("bad"), Some("0;999"), Some("0;"), Some("0;15\x1b")] {
            assert_eq!(resolve(ThemeMode::Auto, None, value), ThemeMode::Dark);
        }
        assert_eq!(resolve(ThemeMode::Auto, None, Some(&"0".repeat(65))), ThemeMode::Dark);
    }
    #[test]
    fn no_color_environment_presence_includes_empty_and_false_values() {
        for value in ["", "0", "false"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "theme::tests::environment_probe", "--ignored"])
                .env("NO_COLOR", value).env("COLORTERM", "truecolor").env("TERM", "xterm-256color")
                .output().unwrap();
            assert!(output.status.success(), "NO_COLOR presence must win: {value:?}");
        }
    }
    #[test]
    #[ignore = "run by the environment isolation parent test"]
    fn environment_probe() {
        assert_eq!(Capability::from_env(), Capability::NoColor);
        assert_eq!(crate::settings::Settings::default().colors.text, Color::Reset);
    }
    #[test]
    fn capability_order_and_no_color() {
        assert_eq!(Capability::detect(true, Some("truecolor"), Some("xterm-256color")), Capability::NoColor);
        assert_eq!(Capability::detect(false, Some("24bit"), Some("dumb")), Capability::TrueColor);
        assert_eq!(Capability::detect(false, None, Some("xterm-256color")), Capability::Ansi256);
        assert_eq!(Capability::detect(false, None, None), Capability::Ansi16);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let t = Theme::new(mode, Capability::NoColor);
            for c in [t.background,t.text,t.muted,t.border,t.border_focus,t.accent,t.success,t.warning,t.danger,t.info,t.selection_fg,t.selection_bg,t.header_bg] { assert_eq!(c, Color::Reset); }
            assert!(t.selected().add_modifier.contains(Modifier::REVERSED));
            assert!(t.border_style(true).add_modifier.contains(Modifier::BOLD));
        }
        assert_eq!(degrade((255,0,0), Capability::Ansi256), Color::Indexed(196));
        assert_eq!(degrade((255,0,0), Capability::Ansi16), Color::LightRed);
    }
    #[test]
    fn degraded_palette_roles_meet_aa_with_reference_terminal_colors() {
        let channel = |color| match color {
            Color::Indexed(i) => xterm_rgb(i),
            Color::Black => ANSI[0], Color::Red => ANSI[1], Color::Green => ANSI[2],
            Color::Yellow => ANSI[3], Color::Blue => ANSI[4], Color::Magenta => ANSI[5],
            Color::Cyan => ANSI[6], Color::Gray => ANSI[7], Color::DarkGray => ANSI[8],
            Color::LightRed => ANSI[9], Color::LightGreen => ANSI[10], Color::LightYellow => ANSI[11],
            Color::LightBlue => ANSI[12], Color::LightMagenta => ANSI[13], Color::LightCyan => ANSI[14],
            Color::White => ANSI[15], _ => panic!("degraded color expected"),
        };
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for capability in [Capability::Ansi16, Capability::Ansi256] {
                let t = Theme::new(mode, capability);
                for bg in [t.background, t.selection_bg] {
                    for fg in [t.text,t.muted,t.border,t.border_focus,t.accent,t.success,t.warning,t.danger,t.info,t.selection_fg] {
                        let (a,b) = (luminance(channel(fg)),luminance(channel(bg)));
                        let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                        assert!(ratio >= 4.5, "{mode:?} {capability:?} {fg:?}/{bg:?}: {ratio}");
                    }
                }
            }
        }
        assert_eq!(adapt(Color::Rgb(1,2,3), Capability::NoColor), Color::Reset);
        assert_eq!(adapt(Color::Indexed(196), Capability::NoColor), Color::Reset);
    }
    #[test]
    fn truecolor_roles_meet_aa_on_normal_and_selection_backgrounds() {
        let lum = |c| if let Color::Rgb(r,g,b) = c { luminance((r,g,b)) } else { panic!("RGB expected") };
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let t = Theme::new(mode, Capability::TrueColor);
            for bg in [t.background, t.selection_bg] {
                for fg in [t.text,t.muted,t.border,t.border_focus,t.accent,t.success,t.warning,t.danger,t.info,t.selection_fg] {
                    let (a,b) = (lum(fg),lum(bg));
                    let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                    assert!(ratio >= 4.5, "{mode:?} {fg:?}/{bg:?}: {ratio}");
                }
            }
        }
    }
}
