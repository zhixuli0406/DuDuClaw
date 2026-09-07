// The `RuntimeAuth` step's provider catalog — WP-C of
// `docs/todo/TODO-ai-runtimes-2026-09.md` (2026-09-05).
//
// Until this round the step collected ONE Anthropic API key. The TODO's
// decision 2 ("首版內建 runtime 範圍：全部") means a freshly installed
// appliance ships a dozen-plus AI CLIs, and OOBE is the one screen where an
// operator is actually asked to hand the machine a credential — so the step
// lists every provider the platform knows how to use, not just the one it
// happened to start with.
//
// ── Why a SHELL-SIDE static table, not `duduclaw_core::provider_env` ─────
// This crate does not depend on `duduclaw-core` (see `Cargo.toml` — its
// whole dependency set is gpui + the native-gui facade + serde/tokio; the
// shell is a separate workspace with its own `Cargo.lock`, deliberately not
// wired into the platform workspace). The table below MIRRORS
// `duduclaw_core::provider_env::KNOWN_PROVIDER_IDS` (the ids `accounts.add`
// accepts as `provider` once WP-A lands) plus the CLI runtimes WP-B adds,
// and the mirror is stated here explicitly so a future divergence is a
// documented drift rather than a silent one. `provider_ids_mirror_core` in
// the test module below pins the KNOWN_PROVIDER_IDS half literally.
//
// ── The `google` / `gemini` duplicate ────────────────────────────────────
// `KNOWN_PROVIDER_IDS` carries both, and `provider_env_key_names` maps them
// onto the SAME pair of env vars (`GEMINI_API_KEY`, `GOOGLE_API_KEY`) — they
// are two spellings of one credential, not two providers. Listing both would
// put two rows on screen that write the same key twice under different ids,
// so this table keeps the canonical `gemini` and says so here rather than
// pretending the duplicate does not exist.
//
// ── Login: fail closed on a runtime the gateway cannot name ──────────────
// `auth.cli_login.start` takes a `runtime` string which the gateway resolves
// through `duduclaw_core::types::RuntimeType::parse`. Before WP-B that
// function DEFAULTED AN UNRECOGNISED STRING TO `Claude` (with a
// `tracing::warn!`, not an error), so sending `runtime: "kimi"` would have
// silently run `claude setup-token` — the operator clicks "登入 Kimi" and is
// asked to authorize an Anthropic account. WP-B made `parse` return `None`
// for unknown ids (the gateway now refuses), and `cli_auth::spec_for` reads
// each runtime's login argv from `duduclaw_core::runtime_catalog`. This
// step still keeps its own explicit allow-list
// ([`ProviderEntry::login_dispatchable`]): it names exactly the catalog
// runtimes whose `LoginMethod` is not `None`, so a provider row whose CLI
// is API-key-only (Qwen, Mistral Vibe) never renders a login button, and a
// gateway older than WP-B is still never sent an id it would misroute.

use crate::i18n::Key;

/// Stable `[[accounts]]` id prefix for a credential entered in OOBE — the
/// dashboard's account list shows the result under this name, so an operator
/// can tell "the one I typed during setup" apart from anything added later.
const OOBE_ACCOUNT_PREFIX: &str = "oobe-";

/// One row of the step's provider list.
///
/// `name` is a PRODUCT name and is deliberately never routed through
/// `crate::i18n` — same policy `LanguageChoice::label()` documents for
/// language names, and the same one the dashboard's own `CliLoginModal`
/// applies to its `RUNTIME_LABELS` table ("Product names — verbatim").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderEntry {
    /// The `provider` value sent with `accounts.add`, and the second half of
    /// the account id (see [`account_id`]). Lowercase ASCII, stable.
    pub(crate) id: &'static str,
    /// What the row shows. Product name, verbatim.
    pub(crate) name: &'static str,
    /// The `runtime` string `auth.cli_login.start` expects, for providers
    /// whose CLI has an interactive login at all. `None` = API key only.
    /// Present does NOT mean startable today — see
    /// [`ProviderEntry::login_dispatchable`].
    pub(crate) login_runtime: Option<&'static str>,
}

impl ProviderEntry {
    /// The one-line note under the row's name. Derived from `login_runtime`
    /// rather than stored per entry: the honest difference between rows IS
    /// "can this one take a subscription login, or only a key", and writing
    /// seventeen near-identical strings per locale would invite exactly the
    /// vendor-specific claims ("logs in with your Moonshot account") this
    /// crate has no way to verify.
    pub(crate) fn note_key(&self) -> Key {
        match self.login_runtime {
            Some(_) => Key::RuntimeProviderNoteLoginOrKey,
            None => Key::RuntimeProviderNoteKeyOnly,
        }
    }

    /// Can this round actually START a login for this provider? See this
    /// file's header comment — a `false` here means the gateway would
    /// silently resolve the runtime to Claude, so the button must not fire.
    pub(crate) fn login_dispatchable(&self) -> bool {
        self.login_runtime.is_some_and(|r| GATEWAY_KNOWN_LOGIN_RUNTIMES.contains(&r))
    }
}

/// Runtime ids whose `duduclaw_core::runtime_catalog` entry carries a
/// `LoginMethod` other than `None` (checked against that table, not
/// assumed), i.e. the ones `duduclaw-gateway::cli_auth::spec_for` answers
/// with a real `CliAuthSpec`. `antigravity` is absent because it is not a
/// provider this step offers; `qwen` (free OAuth retired 2026-04-15) and
/// `vibe` (API key only) are absent because their catalog login is `None`.
const GATEWAY_KNOWN_LOGIN_RUNTIMES: &[&str] =
    &["claude", "codex", "gemini", "grok", "kimi", "copilot", "kiro", "cursor", "opencode"];

/// Every provider the `RuntimeAuth` step offers, in the order it renders
/// them: the four with a first-party agent CLI the platform already drives,
/// then the rest of the CLI runtimes, then the plain API-key providers.
pub(crate) const PROVIDERS: &[ProviderEntry] = &[
    ProviderEntry { id: "anthropic", name: "Claude Code (Anthropic)", login_runtime: Some("claude") },
    ProviderEntry { id: "openai", name: "Codex (OpenAI)", login_runtime: Some("codex") },
    ProviderEntry { id: "gemini", name: "Gemini CLI (Google)", login_runtime: Some("gemini") },
    ProviderEntry { id: "xai", name: "Grok (xAI)", login_runtime: Some("grok") },
    // Qwen's free OAuth was retired 2026-04-15 (TODO §2) — key only, on
    // purpose, not an omission.
    ProviderEntry { id: "qwen", name: "Qwen Code", login_runtime: None },
    ProviderEntry { id: "kimi", name: "Kimi Code", login_runtime: Some("kimi") },
    ProviderEntry { id: "copilot", name: "GitHub Copilot CLI", login_runtime: Some("copilot") },
    ProviderEntry { id: "kiro", name: "Kiro CLI", login_runtime: Some("kiro") },
    ProviderEntry { id: "cursor", name: "Cursor Agent", login_runtime: Some("cursor") },
    // Mistral Vibe authenticates with an API key only (catalog `vibe`:
    // `LoginMethod::None`) — key only, on purpose.
    ProviderEntry { id: "mistral", name: "Mistral Vibe", login_runtime: None },
    ProviderEntry { id: "opencode", name: "OpenCode", login_runtime: Some("opencode") },
    ProviderEntry { id: "deepseek", name: "DeepSeek", login_runtime: None },
    ProviderEntry { id: "minimax", name: "MiniMax", login_runtime: None },
    ProviderEntry { id: "zai", name: "Z.ai GLM", login_runtime: None },
    ProviderEntry { id: "groq", name: "Groq", login_runtime: None },
    ProviderEntry { id: "together", name: "Together AI", login_runtime: None },
    ProviderEntry { id: "openrouter", name: "OpenRouter", login_runtime: None },
];

/// Look one row up by its stable id. `None` for anything not in the table —
/// callers treat that as "no such row", never as a default provider.
pub(crate) fn find(id: &str) -> Option<&'static ProviderEntry> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// The `[[accounts]]` id an OOBE-entered key is stored under. One account
/// per provider, deterministic: re-running OOBE and re-typing the same
/// provider's key hits the SAME id, which the gateway rejects as a duplicate
/// rather than silently accumulating `oobe-anthropic-1`, `-2`, … entries the
/// operator never asked for.
pub(crate) fn account_id(provider: &str) -> String {
    format!("{OOBE_ACCOUNT_PREFIX}{provider}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{t, Locale};

    const LOCALES: [Locale; 3] = [Locale::ZhTw, Locale::En, Locale::JaJp];

    #[test]
    fn every_provider_id_is_unique() {
        let mut seen = Vec::new();
        for p in PROVIDERS {
            assert!(!seen.contains(&p.id), "duplicate provider id `{}` — two rows would write the same account id", p.id);
            seen.push(p.id);
        }
        assert_eq!(seen.len(), PROVIDERS.len());
    }

    #[test]
    fn every_provider_name_is_unique_and_non_empty() {
        // Two rows with the same visible name are indistinguishable to the
        // operator even when their ids differ.
        let mut seen: Vec<&str> = Vec::new();
        for p in PROVIDERS {
            assert!(!p.name.trim().is_empty(), "provider `{}` has no display name", p.id);
            assert!(!seen.contains(&p.name), "duplicate provider name `{}`", p.name);
            seen.push(p.name);
        }
    }

    #[test]
    fn every_provider_id_is_a_safe_lowercase_ascii_slug() {
        // The id becomes both a gpui element id and half of an
        // `[[accounts]]` key — anything but `[a-z0-9-]` would be a surprise
        // in either place.
        for p in PROVIDERS {
            assert!(
                !p.id.is_empty() && p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "provider id `{}` is not a lowercase ascii slug",
                p.id
            );
        }
    }

    #[test]
    fn every_provider_resolves_a_non_empty_note_in_all_three_locales() {
        for p in PROVIDERS {
            for locale in LOCALES {
                let text = t(locale, p.note_key());
                assert!(!text.trim().is_empty(), "provider `{}` has no {:?} note", p.id, locale);
            }
        }
    }

    #[test]
    fn the_two_note_keys_read_differently_so_the_login_row_is_distinguishable() {
        for locale in LOCALES {
            assert_ne!(
                t(locale, Key::RuntimeProviderNoteLoginOrKey),
                t(locale, Key::RuntimeProviderNoteKeyOnly),
                "{locale:?}: a login-capable row must not read identically to a key-only one"
            );
        }
    }

    /// The mirror this file's header comment promises. Written as a literal
    /// list (not derived) precisely so a change to `duduclaw_core::
    /// provider_env::KNOWN_PROVIDER_IDS` that this crate cannot see at
    /// compile time still shows up as a failing assertion the moment someone
    /// updates one side.
    #[test]
    fn provider_ids_mirror_core() {
        // `duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`, verbatim as of
        // 2026-09-05.
        const KNOWN_PROVIDER_IDS: &[&str] =
            &["anthropic", "openai", "gemini", "google", "deepseek", "minimax", "groq", "together", "mistral", "openrouter", "xai", "qwen"];
        for id in KNOWN_PROVIDER_IDS {
            // `google` is the documented exception — see this file's header
            // comment ("The `google` / `gemini` duplicate").
            if *id == "google" {
                assert!(find("google").is_none(), "`google` must stay folded into the canonical `gemini` row");
                continue;
            }
            assert!(find(id).is_some(), "core knows provider `{id}` but the OOBE list has no row for it");
        }
    }

    #[test]
    fn account_ids_derive_from_the_provider_id() {
        assert_eq!(account_id("anthropic"), "oobe-anthropic");
        assert_eq!(account_id("openrouter"), "oobe-openrouter");
        // Every row in the table must produce a distinct account id, or two
        // providers would collide inside `config.toml`'s `[[accounts]]`.
        let mut ids: Vec<String> = PROVIDERS.iter().map(|p| account_id(p.id)).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "two providers derive the same account id");
    }

    #[test]
    fn the_pre_wp_c_anthropic_account_id_is_unchanged() {
        // The single-field step this round replaces stored its key as
        // `oobe-anthropic`. A machine that already ran OOBE has that row in
        // `config.toml`; deriving a DIFFERENT id here would quietly create a
        // second Anthropic account instead of reporting the duplicate.
        assert_eq!(account_id("anthropic"), "oobe-anthropic");
    }

    #[test]
    fn login_is_only_dispatchable_for_runtimes_the_gateway_can_actually_name() {
        // The load-bearing one: a dispatchable-by-default list would send an
        // operator clicking "登入 X" through whatever the gateway does with an
        // id it cannot resolve (pre-WP-B: an ANTHROPIC login), so the button
        // only fires for runtimes the catalog gives a real login flow.
        for id in ["anthropic", "openai", "gemini", "xai", "kimi", "copilot", "kiro", "cursor", "opencode"] {
            let entry = find(id).expect(id);
            assert!(entry.login_runtime.is_some(), "`{id}` offers a login button");
            assert!(entry.login_dispatchable(), "`{id}` must be startable — its catalog login is not `None`");
        }
        for id in ["qwen", "mistral", "deepseek", "minimax", "zai", "groq", "together", "openrouter"] {
            let entry = find(id).expect(id);
            assert_eq!(entry.login_runtime, None, "`{id}` has no CLI login");
            assert!(!entry.login_dispatchable());
        }
    }

    #[test]
    fn every_dispatchable_runtime_is_in_the_allow_list_and_vice_versa() {
        for runtime in GATEWAY_KNOWN_LOGIN_RUNTIMES {
            assert!(
                PROVIDERS.iter().any(|p| p.login_runtime == Some(runtime)),
                "allow-listed runtime `{runtime}` has no provider row — the list would gate nothing"
            );
        }
    }

    #[test]
    fn find_returns_nothing_for_an_unknown_id() {
        assert!(find("").is_none());
        assert!(find("google").is_none());
        assert!(find("not-a-provider").is_none());
    }

    #[test]
    fn the_nine_login_capable_providers_are_exactly_the_catalog_ones() {
        let with_login: Vec<&str> = PROVIDERS.iter().filter(|p| p.login_runtime.is_some()).map(|p| p.id).collect();
        assert_eq!(with_login, vec!["anthropic", "openai", "gemini", "xai", "kimi", "copilot", "kiro", "cursor", "opencode"]);
    }

    #[test]
    fn every_login_runtime_is_dispatchable_now_that_wp_b_landed() {
        // Inverse of the allow-list test: a row that offers a login button
        // but is not dispatchable would render the "not available yet" note
        // forever. After WP-B there is no such row.
        for p in PROVIDERS.iter().filter(|p| p.login_runtime.is_some()) {
            assert!(p.login_dispatchable(), "`{}` has a login_runtime that is not allow-listed", p.id);
        }
    }
}
