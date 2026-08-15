//! Recursive TOML table overlay merge.
//!
//! Relocated from `duduclaw-cli::expert::mod` (WP-6F, agent presets P1) so
//! `duduclaw-core::preset` can use the exact same merge semantics without a
//! reverse crate dependency (`duduclaw-core` cannot depend on
//! `duduclaw-cli`). `duduclaw-cli::expert` now calls this copy instead of
//! keeping its own — per `DESIGN-agent-presets-2026-08.md` §1: "疊層用**已
//! 存在**的 `merge_toml()`…不另造合併語意".
//!
//! # Semantics (pinned by the tests below)
//!
//! - Two tables at the same key: merge recursively.
//! - Anything else (scalar, array, or a type mismatch): the overlay's value
//!   replaces the base's wholesale.
//!
//! The array case is load-bearing for preset R1.3 ("陣列覆寫不合併"): a
//! per-agent `agent.toml` that writes `allowed_tools = []` is expressing
//! "clear this list", not "merge with the preset's list" — and because the
//! overlay here is a raw parse of exactly what the human wrote, a key that
//! was never written at all is simply absent from the overlay table, so the
//! base (preset) value is inherited untouched. No special-casing needed.

/// Recursively merge `overlay` into `base`, overlay winning on every leaf.
pub fn merge_toml(base: &mut toml::value::Table, overlay: &toml::value::Table) {
    for (k, v) in overlay {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(bt)), toml::Value::Table(ot)) => merge_toml(bt, ot),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_toml_recursive() {
        let mut base: toml::value::Table =
            "[model]\npreferred = 'a'\nfallback = 'f'\n".parse::<toml::Table>().unwrap();
        let overlay: toml::value::Table =
            "[model]\npreferred = 'b'\n[budget]\nmonthly_limit_cents = 100\n"
                .parse::<toml::Table>()
                .unwrap();
        merge_toml(&mut base, &overlay);
        let model = base["model"].as_table().unwrap();
        assert_eq!(model["preferred"].as_str(), Some("b")); // overlay wins
        assert_eq!(model["fallback"].as_str(), Some("f")); // base kept
        assert!(base.contains_key("budget")); // new section added
    }

    #[test]
    fn an_empty_array_overlay_clears_rather_than_merges() {
        let mut base: toml::value::Table =
            "[capabilities]\nallowed_tools = [\"Bash\", \"WebFetch\"]\n"
                .parse::<toml::Table>()
                .unwrap();
        let overlay: toml::value::Table =
            "[capabilities]\nallowed_tools = []\n".parse::<toml::Table>().unwrap();
        merge_toml(&mut base, &overlay);
        let caps = base["capabilities"].as_table().unwrap();
        assert_eq!(caps["allowed_tools"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_key_absent_from_the_overlay_is_inherited_from_base_untouched() {
        let mut base: toml::value::Table =
            "[capabilities]\nallowed_tools = [\"Bash\"]\n".parse::<toml::Table>().unwrap();
        let overlay: toml::value::Table = "[capabilities]\n".parse::<toml::Table>().unwrap();
        merge_toml(&mut base, &overlay);
        let caps = base["capabilities"].as_table().unwrap();
        assert_eq!(caps["allowed_tools"].as_array().unwrap().len(), 1);
    }
}
