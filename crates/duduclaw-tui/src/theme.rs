//! Semantic presentation tokens. No gateway text or terminal replies are emitted here.
use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    Auto,
    Light,
    Dark,
    Ansi,
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

/// Whether the two large canvas regions use the theme or terminal background.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Background {
    #[default]
    Theme,
    Terminal,
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub capability: Capability,
    pub source_capability: Capability,
    pub app_shell: Color,
    pub page_canvas: Color,
    /// The current painting surface (page canvas by default).
    pub background: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub hover: Color,
    pub text: Color,
    pub secondary: Color,
    pub muted: Color,
    pub border: Color,
    pub border_focus: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub brand: Color,
    pub idle: Color,
    pub paused: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub primary_fg: Color,
    pub primary_bg: Color,
    pub warning_fg: Color,
    pub warning_bg: Color,
    pub header_bg: Color,
    terminal_rgb: (u8, u8, u8),
}
impl Theme {
    pub fn new(mode: ThemeMode, capability: Capability) -> Self {
        Self::configured(mode, capability, Background::Theme, None, None)
    }
    pub fn configured(mode: ThemeMode, source_capability: Capability, background: Background,
        terminal_rgb: Option<(u8, u8, u8)>, colorfgbg: Option<&str>) -> Self {
        let resolved = resolve(if mode == ThemeMode::Ansi { ThemeMode::Auto } else { mode }, terminal_rgb, colorfgbg);
        let light = resolved == ThemeMode::Light;
        let capability = if mode == ThemeMode::Ansi && source_capability != Capability::NoColor {
            Capability::Ansi16
        } else { source_capability };
        let map = |hex| degrade(rgb(hex), capability);
        let terminal_layers = mode == ThemeMode::Ansi;
        let terminal_rgb = terminal_rgb.or_else(|| colorfgbg.filter(|s| s.len() <= 64)
            .and_then(|s| s.rsplit(';').next()).and_then(|s| s.parse::<u8>().ok()).map(xterm_rgb))
            .unwrap_or(if light { (255,255,255) } else { (0,0,0) });
        let mut theme = if capability == Capability::Ansi16 {
            let (bg, text, muted, accent, success, danger, info, brand) = if light {
                (Color::White, Color::Black, Color::Black, Color::Red, Color::Green, Color::Magenta, Color::Blue, Color::Blue)
            } else {
                (Color::Black, Color::White, Color::Gray, Color::LightYellow, Color::LightGreen, Color::LightRed, Color::LightCyan, Color::LightCyan)
            };
            Self { capability, source_capability, app_shell: bg, page_canvas: bg, background: bg,
                surface: bg, surface_raised: bg, hover: bg, text, secondary: muted, muted,
                border: if light { Color::Gray } else { Color::DarkGray }, border_focus: accent,
                accent, success, warning: accent, danger, info, brand, idle: muted, paused: muted,
                selection_fg: text, selection_bg: bg, primary_fg: bg, primary_bg: text,
                warning_fg: bg, warning_bg: accent, header_bg: bg, terminal_rgb }
        } else {
            let (shell, canvas, surface, raised, hover, selected, border, text, secondary, muted,
                success, warning, danger, info, brand, idle, paused, primary_fg, primary_bg, warning_fg, warning_bg) = if light {
                (0xF4F4F5, 0xFBFBFB, 0xFFFFFF, 0xFFFFFF, 0xF4F4F5, 0xE4E4E7, 0xE4E4E7,
                 0x09090B, 0x3F3F46, 0x71717A, 0x1E7F45, 0x8A5A10, 0xC8102E, 0x2563EB,
                 0x102B48, 0x71717A, 0x71717A, 0xF5F4F0, 0x102B48, 0x8A5A10, 0xF6E7CF)
            } else {
                (0x0D0D10, 0x131316, 0x18181B, 0x1E1E22, 0x27272A, 0x2E2E33, 0x2A2A2E,
                 0xFAFAFA, 0xD4D4D8, 0xA1A1AA, 0x3FD49A, 0xE0A54F, 0xF0657C, 0x5B9BF5,
                 0xBED4EB, 0x8B8E96, 0x6E717A, 0x102B48, 0xF5F4F0, 0x11110F, 0xE0A54F)
            };
            Self { capability, source_capability, app_shell: map(shell), page_canvas: map(canvas), background: map(canvas),
                surface: map(surface), surface_raised: map(raised), hover: map(hover),
                text: map(text), secondary: map(secondary), muted: map(muted), border: map(border),
                border_focus: map(warning), accent: map(warning), success: map(success), warning: map(warning),
                danger: map(danger), info: map(info), brand: map(brand), idle: map(idle), paused: map(paused),
                selection_fg: map(text), selection_bg: map(selected), primary_fg: map(primary_fg), primary_bg: map(primary_bg),
                warning_fg: map(warning_fg), warning_bg: map(warning_bg), header_bg: map(canvas), terminal_rgb }
        };
        if terminal_layers || background == Background::Terminal {
            theme.app_shell = Color::Reset;
            theme.page_canvas = Color::Reset;
            theme.background = Color::Reset;
            theme.header_bg = Color::Reset;
        }
        if terminal_layers {
            theme.surface = Color::Reset;
            theme.surface_raised = Color::Reset;
            theme.hover = Color::Reset;
            theme.selection_bg = Color::Reset;
        }
        theme
    }
    /// Preserve the dashboard tokens; adjust only an insufficient foreground/surface pair.
    /// ANSI checks use reference colors. A user-customized terminal palette is not measurable.
    pub fn readable(self, foreground: Color, background: Color) -> Color {
        let Some(fg) = color_rgb(foreground) else { return foreground; };
        let bg = color_rgb(background).unwrap_or(self.terminal_rgb);
        if contrast(fg, bg) >= 4.5 { return foreground; }
        match self.capability {
            Capability::NoColor => Color::Reset,
            Capability::Ansi256 => Color::Indexed((16..=255).filter(|&i| contrast(xterm_rgb(i), bg) >= 4.5)
                .min_by_key(|&i| distance(fg, xterm_rgb(i))).unwrap_or(16)),
            Capability::Ansi16 => {
                let index = (0..16).filter(|&i| contrast(ANSI[i], bg) >= 4.5)
                    .min_by_key(|&i| distance(fg, ANSI[i])).unwrap_or(0);
                degrade(ANSI[index], Capability::Ansi16)
            }
            Capability::TrueColor => {
                let end = if contrast((0,0,0), bg) > contrast((255,255,255), bg) { 0 } else { 255 };
                for amount in 1..=255 {
                    let mix = |c: u8| ((u32::from(c) * (255 - amount) + end * amount) / 255) as u8;
                    let candidate = (mix(fg.0), mix(fg.1), mix(fg.2));
                    if contrast(candidate, bg) >= 4.5 { return degrade(candidate, Capability::TrueColor); }
                }
                foreground
            }
        }
    }
    pub fn readable_style(self, style: Style) -> Style {
        match (style.fg, style.bg) {
            (Some(fg), Some(bg)) => style.fg(self.readable(fg, bg)),
            _ => style,
        }
    }
    #[cfg(test)]
    pub(crate) fn contrast_ratio(self, foreground: Color, background: Color) -> Option<f64> {
        color_rgb(foreground).map(|fg| contrast(fg, color_rgb(background).unwrap_or(self.terminal_rgb)))
    }
    pub fn on(self, background: Color) -> Self { Self { background, ..self } }
    pub fn foreground(self, color: Color) -> Style {
        Style::default().fg(self.readable(color, self.background)).bg(self.background)
    }
    pub fn base(self) -> Style { self.foreground(self.text) }
    pub fn muted(self) -> Style { self.foreground(self.muted) }
    pub fn emphasis(self, color: Color) -> Style { self.foreground(color).add_modifier(Modifier::BOLD) }
    pub fn selected(self) -> Style { self.selection(true) }
    pub fn selection(self, focused: bool) -> Style {
        if matches!(self.capability, Capability::NoColor | Capability::Ansi16) {
            self.base().add_modifier(if focused { Modifier::REVERSED } else { Modifier::UNDERLINED })
        } else {
            self.on(if focused { self.selection_bg } else { self.hover }).foreground(self.selection_fg)
        }
    }
    pub fn primary(self) -> Style {
        if self.capability == Capability::NoColor { self.base().add_modifier(Modifier::REVERSED | Modifier::BOLD) }
        else { self.on(self.primary_bg).foreground(self.primary_fg) }
    }
    pub fn warning_pill(self, filled: bool) -> Style {
        if filled && self.capability == Capability::NoColor { self.base().add_modifier(Modifier::REVERSED | Modifier::BOLD) }
        else if filled { self.on(self.warning_bg).foreground(self.warning_fg) }
        else { self.foreground(self.warning) }
    }
    pub fn border_style(self, focused: bool) -> Style {
        if focused { self.emphasis(self.border_focus) } else { Style::default().fg(self.border).bg(self.background) }
    }
}

fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    Some(match color {
        Color::Reset => return None,
        Color::Rgb(r,g,b) => (r,g,b), Color::Indexed(i) => xterm_rgb(i),
        Color::Black => ANSI[0], Color::Red => ANSI[1], Color::Green => ANSI[2], Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4], Color::Magenta => ANSI[5], Color::Cyan => ANSI[6], Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8], Color::LightRed => ANSI[9], Color::LightGreen => ANSI[10], Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12], Color::LightMagenta => ANSI[13], Color::LightCyan => ANSI[14], Color::White => ANSI[15],
    })
}
fn contrast(a: (u8,u8,u8), b: (u8,u8,u8)) -> f64 {
    let (a,b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
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
    fn all_text_roles_meet_aa_on_every_surface() {
        for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Ansi] {
            for capability in [Capability::TrueColor, Capability::Ansi256, Capability::Ansi16] {
                let t = Theme::new(mode, capability);
                for bg in [t.app_shell, t.page_canvas, t.surface, t.surface_raised, t.hover, t.selection_bg] {
                    for fg in [t.text, t.secondary, t.muted, t.accent, t.success, t.warning, t.danger, t.info, t.brand, t.idle, t.paused] {
                        let actual = t.readable(fg, bg);
                        let ratio = contrast(color_rgb(actual).unwrap(), color_rgb(bg).unwrap_or(t.terminal_rgb));
                        assert!(ratio >= 4.5, "{mode:?} {capability:?} {actual:?}/{bg:?}: {ratio}");
                    }
                }
                for (fg,bg) in [(t.primary_fg,t.primary_bg), (t.warning_fg,t.warning_bg)] {
                    assert!(contrast(color_rgb(t.readable(fg,bg)).unwrap(), color_rgb(bg).unwrap()) >= 4.5);
                }
            }
        }
    }
    #[test]
    fn dashboard_surfaces_and_faint_ring_are_preserved() {
        for (mode, shell, canvas, surface, raised, border) in [
            (ThemeMode::Dark, 0x0D0D10, 0x131316, 0x18181B, 0x1E1E22, 0x2A2A2E),
            (ThemeMode::Light, 0xF4F4F5, 0xFBFBFB, 0xFFFFFF, 0xFFFFFF, 0xE4E4E7),
        ] {
            let t = Theme::new(mode, Capability::TrueColor);
            for (color, value) in [(t.app_shell,shell), (t.page_canvas,canvas), (t.surface,surface), (t.surface_raised,raised), (t.border,border)] {
                assert_eq!(color_rgb(color), Some(rgb(value)));
            }
            assert!(contrast(rgb(border), rgb(canvas)) < 1.5);
        }
    }
    #[test]
    fn ansi_and_terminal_background_preserve_capability_and_contrast() {
        for background in [(0,0,0), (255,255,255), (120,120,120)] {
            let t = Theme::configured(ThemeMode::Ansi, Capability::TrueColor, Background::Theme, Some(background), None);
            assert_eq!(t.capability, Capability::Ansi16);
            assert_eq!(t.source_capability, Capability::TrueColor);
            assert_eq!(t.page_canvas, Color::Reset);
            assert_eq!(t.app_shell, Color::Reset);
            assert_eq!(t.surface, Color::Reset);
            for fg in [t.text,t.muted,t.brand,t.success,t.warning,t.danger,t.info] {
                assert!(contrast(color_rgb(t.readable(fg, Color::Reset)).unwrap(), background) >= 4.5);
            }
        }
        let t = Theme::configured(ThemeMode::Dark, Capability::TrueColor, Background::Terminal, None, None);
        assert_eq!(t.page_canvas, Color::Reset);
        assert_ne!(t.surface, Color::Reset);
        assert_eq!(adapt(Color::Rgb(1,2,3), Capability::NoColor), Color::Reset);
        assert_eq!(adapt(Color::Indexed(196), Capability::NoColor), Color::Reset);
    }
}
