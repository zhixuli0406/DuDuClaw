// Minimal i18n layer for the native GUI: a `Locale` enum + a `t(locale, key)`
// lookup over three embedded flat-string JSON catalogs.
//
// This deliberately mirrors `web/src/i18n/index.ts`'s shape (a locale store +
// a lookup over loaded JSON maps) but is NOT a copy of the web catalogs — it
// carries a curated SUBSET of keys actually used by the Phase 1a screens
// (see `web/src/i18n/{zh-TW,en,ja-JP}.json` for the full source of truth),
// plus a handful of new `native.*` keys for screens the web dashboard has no
// equivalent of (the language picker; this is a desktop app, not a
// browser-locale-detected SPA, so language is chosen explicitly on first
// launch instead of sniffed from `navigator.language`).
//
// English is the fallback catalog: a key missing from zh-TW or ja-JP falls
// back to the English string rather than showing a raw message id (same
// policy `web/src/i18n/index.ts` documents for ja-JP: "English fills any key
// missing from the Japanese catalogue").

use std::collections::HashMap;
use std::sync::OnceLock;

use gpui::SharedString;

const ZH_TW_JSON: &str = include_str!("zh-TW.json");
const EN_JSON: &str = include_str!("en.json");
const JA_JP_JSON: &str = include_str!("ja-JP.json");

/// The three languages the language-picker screen offers, matching
/// `web/src/i18n/index.ts`'s `localeNames` (`繁體中文` / `English` / `日本語`)
/// and `defaultLocale = 'zh-TW'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    ZhTw,
    En,
    JaJp,
}

impl Locale {
    /// All selectable locales, in the order the language picker renders them.
    pub const ALL: [Locale; 3] = [Locale::ZhTw, Locale::En, Locale::JaJp];

    /// BCP-47-ish code, matching the web app's `localStorage` values and
    /// catalog file names. S2 uses this as the on-disk value in
    /// `~/.duduclaw/native-gui.toml`'s `locale = "..."` line — see
    /// `config.rs::save_locale`/`load_locale`.
    pub fn code(self) -> &'static str {
        match self {
            Locale::ZhTw => "zh-TW",
            Locale::En => "en",
            Locale::JaJp => "ja-JP",
        }
    }

    /// The language's own name, written in itself — what the language
    /// picker shows (never translated: a person who reads no zh-TW still
    /// needs to recognize "English" and "日本語").
    pub fn native_name(self) -> &'static str {
        match self {
            Locale::ZhTw => "繁體中文",
            Locale::En => "English",
            Locale::JaJp => "日本語",
        }
    }
}

impl Default for Locale {
    /// `web/src/i18n/index.ts`'s `defaultLocale = 'zh-TW'`. Only used before
    /// the language-picker screen commits an explicit choice (e.g. as the
    /// `RootView` field initializer) — since the picker is the very first
    /// screen, nothing actually renders text with this default before the
    /// user picks.
    fn default() -> Self {
        Locale::ZhTw
    }
}

type Catalog = HashMap<String, String>;

fn parse_catalog(json: &str, name: &str) -> Catalog {
    serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("duduclaw-native-gui: malformed i18n catalog {name}: {e}"))
}

fn catalogs() -> &'static HashMap<Locale, Catalog> {
    static CATALOGS: OnceLock<HashMap<Locale, Catalog>> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert(Locale::ZhTw, parse_catalog(ZH_TW_JSON, "zh-TW.json"));
        m.insert(Locale::En, parse_catalog(EN_JSON, "en.json"));
        m.insert(Locale::JaJp, parse_catalog(JA_JP_JSON, "ja-JP.json"));
        m
    })
}

/// Look up `key` in `locale`'s catalog, falling back to English, falling
/// back to the raw key (never panics on a missing key — a missing
/// translation should degrade visibly, not crash the app).
pub fn t(locale: Locale, key: &str) -> SharedString {
    let cats = catalogs();
    if let Some(v) = cats.get(&locale).and_then(|c| c.get(key)) {
        return v.clone().into();
    }
    if let Some(v) = cats.get(&Locale::En).and_then(|c| c.get(key)) {
        return v.clone().into();
    }
    key.to_string().into()
}

/// `t()` with a single `{param}` substitution (only `native.shell.pageStub`
/// needs this in Phase 1a — a hand-rolled `replace` is plenty; no need for a
/// templating dependency).
pub fn t1(locale: Locale, key: &str, param: &str, value: &str) -> SharedString {
    t(locale, key)
        .replace(&format!("{{{param}}}"), value)
        .into()
}

/// `t()` with N `{param}` substitutions — S4b's dashboard needs multi-value
/// strings (e.g. `native.home.card.budget.detail`: "已用 {percent}%（NT$
/// {spent} / {total}）", three params). Generalizes [`t1`] rather than
/// duplicating it; applies substitutions in the order given and is a no-op
/// for any pair whose placeholder isn't present in the catalog string.
pub fn tn(locale: Locale, key: &str, pairs: &[(&str, &str)]) -> SharedString {
    let mut s = t(locale, key).to_string();
    for (param, value) in pairs {
        s = s.replace(&format!("{{{param}}}"), value);
    }
    s.into()
}
