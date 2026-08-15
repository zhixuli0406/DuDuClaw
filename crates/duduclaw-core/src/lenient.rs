//! Type-tolerant serde helpers for `agent.toml` sections that were historically
//! read with hand-rolled `toml::Value` accessors.
//!
//! # Why this exists
//!
//! The pre-unification "shadow readers" reached into a parsed [`toml::Value`]
//! with chains like:
//!
//! ```ignore
//! value.get("runtime").and_then(|r| r.get("provider")).and_then(|s| s.as_str())
//! ```
//!
//! Every link in that chain is *type-tolerant*: a wrong-typed key
//! (`provider = 42`), a wrong-typed section (`runtime = "x"`), or a missing
//! table all silently degrade to the field's documented default. The value is
//! ignored; nothing fails.
//!
//! A naive migration of those keys into the typed [`crate::types::AgentConfig`]
//! would make them **strict**: `provider = 42` would abort the whole
//! `AgentConfig` deserialization, and because
//! `AgentRegistry::load_agent` treats a parse error as fatal, the agent would
//! vanish from the registry entirely. A silently-ignored typo would become
//! total agent loss — a strictly worse failure mode than the one we started
//! with, and exactly the kind of drift the R5 discipline forbids.
//!
//! These helpers keep the typed schema as tolerant as the `toml::Value` chains
//! it replaces. They are format-agnostic (implemented with `#[serde(untagged)]`
//! + [`serde::de::IgnoredAny`], not a `toml::Value` intermediate), so
//! `AgentConfig` still round-trips through JSON for the dashboard.
//!
//! Tolerance is deliberately **one-directional**: it only ever turns a
//! wrong-typed value into the default. It never coerces (`"true"` does not
//! become `true`, `"42"` does not become `42`) — the shadow readers did not
//! coerce either.

use std::fmt;

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// Either the requested type, or "something else entirely, which we ignore".
///
/// `IgnoredAny` accepts any input, so as an untagged fallback arm it turns a
/// type mismatch into a value instead of an error.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Value(T),
    Other(serde::de::IgnoredAny),
}

impl<T> Lenient<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Lenient::Value(v) => Some(v),
            Lenient::Other(_) => None,
        }
    }
}

/// Deserialize `Option<T>`, mapping a wrong-typed value to `None`.
///
/// Reproduces `value.get(key).and_then(|v| v.as_<type>())`: absent ⇒ `None`,
/// present-but-wrong-type ⇒ `None`.
pub fn opt<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Lenient<T>>::deserialize(d)?.and_then(Lenient::into_option))
}

/// Deserialize `T`, mapping a wrong-typed value to [`Default::default`].
///
/// Used at *section* granularity so a scalar where a table was expected
/// (`runtime = "claude"` instead of `[runtime]`) degrades to the section's
/// defaults rather than failing the whole file — matching
/// `value.get("runtime").and_then(|r| r.as_table())` returning `None`.
pub fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Lenient::<T>::deserialize(d)?
        .into_option()
        .unwrap_or_default())
}

/// Deserialize `Option<f64>` accepting **only** a real float — an integer
/// literal yields `None`, not `1i64 as f64`.
///
/// [`opt`] cannot be used for floats. Its untagged fallback delegates to
/// serde's own `f64` impl, which *accepts* an integer and widens it. The
/// readers this replaces called `toml::Value::as_float()`, which returns
/// `None` for `Value::Integer` — so `default_budget_usd = 1` was silently
/// ignored and fell back to the default. Widening it here would have doubled
/// the effective spend ceiling of any config written that way: a real
/// behavior change smuggled in by a schema refactor, which is exactly what
/// the R5 discipline forbids. Preserving the quirk keeps the migration
/// behavior-neutral; correcting it stays available as a separate, deliberate
/// decision.
pub fn opt_float_strict<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    struct FloatOnly;

    impl<'de> Visitor<'de> for FloatOnly {
        type Value = Option<f64>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a floating-point number (an integer is treated as unset)")
        }

        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_f32<E>(self, v: f32) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }

        // Every non-float degrades to "unset", mirroring `as_float()`.
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i128<E>(self, _: i128) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u128<E>(self, _: u128) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D2>(self, d: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            d.deserialize_any(FloatOnly)
        }
        fn visit_map<A>(self, mut m: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_seq<A>(self, mut s: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            while s.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }

    d.deserialize_any(FloatOnly)
}

/// Deserialize a `Vec<String>`, dropping non-string elements.
///
/// Reproduces `arr.iter().filter_map(|v| v.as_str()).map(String::from)`:
/// a non-array value yields an empty vec, and a mixed array keeps only its
/// string elements rather than failing.
pub fn string_vec<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<Lenient<Vec<Lenient<String>>>> = Option::deserialize(d)?;
    Ok(raw
        .and_then(Lenient::into_option)
        .map(|items| items.into_iter().filter_map(Lenient::into_option).collect())
        .unwrap_or_default())
}

/// Deserialize an ordered `key -> string` table, dropping non-string values.
///
/// Reproduces `v.as_table()` followed by
/// `for (k, raw) in tbl { let Some(raw) = raw.as_str() else { continue }; … }`:
/// a missing or non-table value yields an empty vec, and a value of the wrong
/// type inside the table is skipped rather than failing the parse.
///
/// The result is a `Vec` of pairs, not a map, because the callers this
/// replaces build ordered pair lists (process env / HTTP headers). Order is
/// [`BTreeMap`](std::collections::BTreeMap) key order, which is exactly what
/// iterating a `toml::Table` gave (the `toml` crate's table is a `BTreeMap`
/// unless its `preserve_order` feature is enabled, which this workspace does
/// not enable) — so the order-sensitive "first unresolved credential wins the
/// warning" behavior is unchanged.
pub fn string_map<'de, D>(d: D) -> Result<Vec<(String, String)>, D::Error>
where
    D: Deserializer<'de>,
{
    type Raw = std::collections::BTreeMap<String, Lenient<String>>;
    let raw: Option<Lenient<Raw>> = Option::deserialize(d)?;
    Ok(raw
        .and_then(Lenient::into_option)
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.into_option().map(|v| (k, v)))
                .collect()
        })
        .unwrap_or_default())
}

/// Three-state read of one key: absent, well-typed, or present-but-wrong-type.
///
/// [`opt`] collapses the first and the third into `None`, which is right for
/// the many readers whose missing-key and wrong-type behavior coincide. A few
/// readers **must** tell them apart:
///
/// * `budget::load_budget_limits` logs a loud `warn!` for a wrong-typed
///   `[budget]` key ("a config typo must not silently disable a cost
///   control") but stays silent for an absent one;
/// * `mcp_external`'s `allowed_tools` / `denied_tools` are security-relevant:
///   absent means "no filter", while present-but-wrong-type must **skip the
///   whole server** — collapsing it to "no filter" would silently widen the
///   exposed tool surface.
///
/// Folding either into a two-state `Option` would change behavior, so the
/// distinction is carried in the type instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tri<T> {
    /// The key was not present at all.
    Absent,
    /// The key was present and deserialized as `T`.
    Value(T),
    /// The key was present but held some other type; the value is discarded.
    WrongType,
}

impl<T> Default for Tri<T> {
    /// `Absent` — a hand-written impl rather than `#[derive(Default)]`, which
    /// would add an unwanted `T: Default` bound.
    fn default() -> Self {
        Tri::Absent
    }
}

impl<T> Tri<T> {
    /// The well-typed value, if any. Both `Absent` and `WrongType` map to
    /// `None` — i.e. exactly what [`opt`] would have produced.
    pub fn value(self) -> Option<T> {
        match self {
            Tri::Value(v) => Some(v),
            _ => None,
        }
    }

    /// True when the key was present but of the wrong type (the case callers
    /// warn about).
    pub fn is_wrong_type(&self) -> bool {
        matches!(self, Tri::WrongType)
    }
}

/// Deserialize a [`Tri`]. Use with `#[serde(default)]` on the containing
/// struct so an absent key yields [`Tri::Absent`] without calling this.
pub fn tri<'de, D, T>(d: D) -> Result<Tri<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match Option::<Lenient<T>>::deserialize(d)? {
        None => Tri::Absent,
        Some(Lenient::Value(v)) => Tri::Value(v),
        Some(Lenient::Other(_)) => Tri::WrongType,
    })
}

/// Deserialize a [`Tri<Vec<String>>`](Tri) that keeps non-string elements'
/// tolerance while still flagging a non-array value.
///
/// Reproduces `mcp_external::tool_list_field` exactly: absent ⇒ [`Tri::Absent`];
/// an array ⇒ [`Tri::Value`] of only its string elements (a mixed array is
/// filtered, NOT rejected — `str_array` used `filter_map`); anything else ⇒
/// [`Tri::WrongType`], which the caller turns into a fail-closed skip.
///
/// A plain `tri::<Vec<String>>` would misclassify the mixed array as
/// `WrongType` and skip a server that used to load.
pub fn tri_string_vec<'de, D>(d: D) -> Result<Tri<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        match Option::<Lenient<Vec<Lenient<String>>>>::deserialize(d)? {
            None => Tri::Absent,
            Some(Lenient::Value(items)) => {
                Tri::Value(items.into_iter().filter_map(Lenient::into_option).collect())
            }
            Some(Lenient::Other(_)) => Tri::WrongType,
        },
    )
}

/// Deserialize a `Vec<T>`, replacing a wrong-typed **element** with
/// `T::default()` and a wrong-typed whole value with an empty vec.
///
/// For arrays of tables read element-by-element with `entry.get(..)` chains: a
/// non-table element made every lookup return `None`, which is precisely what
/// an all-defaults `T` produces. Failing the parse instead would lose every
/// well-formed sibling element.
pub fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let raw: Option<Lenient<Vec<Lenient<T>>>> = Option::deserialize(d)?;
    Ok(raw
        .and_then(Lenient::into_option)
        .map(|items| {
            items
                .into_iter()
                .map(|e| e.into_option().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default())
}

/// A TOML number that keeps its integer-vs-float identity.
///
/// The readers this serves treat the two differently — `budget` rounds a
/// float but clamps an integer, and both branches are reachable from real
/// configs (`monthly_limit_cents = 100.0` is a documented common typo). A
/// plain `f64` would lose the distinction (and, above 2^53, precision), so
/// the variants are preserved and the arithmetic stays in the accessor.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TomlNumber {
    /// Matched first, so an integer literal never lands in [`Self::Float`].
    Int(i64),
    Float(f64),
}

/// A TOML boolean that also accepts an integer, keeping the two apart.
///
/// `budget`'s `hard_stop` tolerates `hard_stop = 1` as a common mistake but
/// `warn!`s about it, so "was a bool" and "was an int" must remain
/// distinguishable at the accessor.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TomlFlag {
    /// Matched first, so a real bool never lands in [`Self::Int`].
    Bool(bool),
    Int(i64),
}

/// Deserialize `Option<f64>` accepting an integer literal too, widening it.
///
/// The counterpart of [`opt_float_strict`], for the readers that wrote
/// `as_float().or_else(|| as_integer().map(|i| i as f64))` — i.e. those that
/// deliberately DID accept `aee_settle_hours = 24`. Keeping both helpers (and
/// naming which is which at each call site) is how the two opposite
/// historical quirks stay pinned instead of quietly converging.
pub fn opt_number_lossy<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        Option::<Lenient<TomlNumber>>::deserialize(d)?
            .and_then(Lenient::into_option)
            .map(|n| match n {
                TomlNumber::Int(i) => i as f64,
                TomlNumber::Float(f) => f,
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct Section {
        #[serde(deserialize_with = "opt")]
        name: Option<String>,
        #[serde(deserialize_with = "opt")]
        flag: Option<bool>,
        #[serde(deserialize_with = "opt")]
        count: Option<i64>,
        #[serde(deserialize_with = "string_vec")]
        list: Vec<String>,
    }

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct Doc {
        #[serde(deserialize_with = "or_default")]
        section: Section,
    }

    fn doc(s: &str) -> Doc {
        toml::from_str(s).expect("lenient schema must never fail to parse")
    }

    #[test]
    fn well_typed_values_round_trip() {
        let d = doc("[section]\nname = \"a\"\nflag = true\ncount = 7\nlist = [\"x\", \"y\"]\n");
        assert_eq!(d.section.name.as_deref(), Some("a"));
        assert_eq!(d.section.flag, Some(true));
        assert_eq!(d.section.count, Some(7));
        assert_eq!(d.section.list, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn wrong_typed_scalars_degrade_to_none_not_error() {
        // The shadow readers' `.and_then(|v| v.as_str())` returned None here.
        let d = doc("[section]\nname = 42\nflag = \"yes\"\ncount = \"7\"\n");
        assert_eq!(d.section.name, None);
        assert_eq!(d.section.flag, None);
        assert_eq!(d.section.count, None);
    }

    #[test]
    fn wrong_typed_section_degrades_to_defaults_not_error() {
        // `value.get("section").and_then(|s| s.as_table())` was None here.
        assert_eq!(doc("section = \"scalar\"\n").section, Section::default());
    }

    #[test]
    fn mixed_array_keeps_only_strings() {
        // Mirrors `filter_map(|v| v.as_str())` — 1 and true are dropped, not fatal.
        let d = doc("[section]\nlist = [\"a\", 1, true, \"b\"]\n");
        assert_eq!(d.section.list, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn non_array_list_is_empty() {
        assert!(doc("[section]\nlist = \"nope\"\n").section.list.is_empty());
    }

    #[test]
    fn absent_everything_is_default() {
        assert_eq!(doc(""), Doc::default());
    }

    #[test]
    fn no_coercion_between_types() {
        // Tolerance must not become coercion: "true" stays unset, not `true`.
        let d = doc("[section]\nflag = \"true\"\ncount = 1.5\n");
        assert_eq!(d.section.flag, None);
        assert_eq!(d.section.count, None);
    }

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct Money {
        #[serde(deserialize_with = "opt_float_strict")]
        amount: Option<f64>,
    }

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct MoneyDoc {
        #[serde(deserialize_with = "or_default")]
        m: Money,
    }

    fn money(s: &str) -> Money {
        toml::from_str::<MoneyDoc>(s).expect("must never fail to parse").m
    }

    #[test]
    fn strict_float_accepts_floats_only() {
        assert_eq!(money("[m]\namount = 1.5\n").amount, Some(1.5));
        assert_eq!(money("[m]\namount = 0.0\n").amount, Some(0.0));
        assert_eq!(money("[m]\namount = -2.25\n").amount, Some(-2.25));
    }

    #[test]
    fn strict_float_treats_integer_literal_as_unset() {
        // This is the whole point: serde's own `f64` impl would widen `1` to
        // `1.0`, but `toml::Value::as_float()` returned None — and the fork
        // budget readers depended on that. Widening would silently raise a
        // spend ceiling.
        assert_eq!(money("[m]\namount = 1\n").amount, None);
        assert_eq!(money("[m]\namount = 0\n").amount, None);
        assert_eq!(money("[m]\namount = -3\n").amount, None);
    }

    #[test]
    fn strict_float_degrades_other_types_without_failing() {
        for body in [
            "[m]\namount = \"1.5\"\n",
            "[m]\namount = true\n",
            "[m]\namount = [1.5]\n",
            "[m]\n",
            "m = \"scalar\"\n",
            "",
        ] {
            assert_eq!(money(body).amount, None, "for {body:?}");
        }
    }

    #[test]
    fn strict_float_survives_the_untagged_section_path() {
        // `or_default` buffers the section through serde's untagged Content;
        // the integer/float distinction must survive that round-trip, since
        // that is the path `ForkSection` actually takes.
        assert_eq!(money("[m]\namount = 2.5\n").amount, Some(2.5));
        assert_eq!(money("[m]\namount = 2\n").amount, None);
    }

    // ── Tri / number / map helpers ───────────────────────────────────────

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct TriDoc {
        #[serde(deserialize_with = "tri")]
        num: Tri<TomlNumber>,
        #[serde(deserialize_with = "tri")]
        flag: Tri<TomlFlag>,
        #[serde(deserialize_with = "tri")]
        list: Tri<Vec<String>>,
    }

    fn tri_doc(s: &str) -> TriDoc {
        toml::from_str(s).expect("lenient schema must never fail to parse")
    }

    #[test]
    fn tri_separates_absent_from_wrong_type() {
        let absent = tri_doc("");
        assert_eq!(absent.num, Tri::Absent);
        assert!(!absent.num.is_wrong_type());

        let wrong = tri_doc("num = \"lots\"\n");
        assert_eq!(wrong.num, Tri::WrongType);
        assert!(wrong.num.is_wrong_type());
        // Both still read as "no usable value" for callers that don't care.
        assert_eq!(absent.num.value(), None);
        assert_eq!(wrong.num.value(), None);
    }

    #[test]
    fn tri_number_keeps_integer_and_float_apart() {
        assert_eq!(tri_doc("num = 100\n").num, Tri::Value(TomlNumber::Int(100)));
        assert_eq!(
            tri_doc("num = 100.0\n").num,
            Tri::Value(TomlNumber::Float(100.0))
        );
    }

    #[test]
    fn tri_flag_keeps_bool_and_integer_apart() {
        assert_eq!(tri_doc("flag = true\n").flag, Tri::Value(TomlFlag::Bool(true)));
        assert_eq!(tri_doc("flag = 1\n").flag, Tri::Value(TomlFlag::Int(1)));
        assert_eq!(tri_doc("flag = \"yes\"\n").flag, Tri::WrongType);
    }

    #[test]
    fn tri_list_flags_a_scalar_where_an_array_was_expected() {
        // The security-relevant case: `allowed_tools = "x"` must be
        // distinguishable from an absent key, never collapse to "empty".
        assert_eq!(tri_doc("list = [\"a\"]\n").list, Tri::Value(vec!["a".into()]));
        assert_eq!(tri_doc("list = \"a\"\n").list, Tri::WrongType);
        assert_eq!(tri_doc("").list, Tri::Absent);
    }

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct MapDoc {
        #[serde(deserialize_with = "string_map")]
        env: Vec<(String, String)>,
    }

    fn map_doc(s: &str) -> Vec<(String, String)> {
        toml::from_str::<MapDoc>(s)
            .expect("must never fail to parse")
            .env
    }

    #[test]
    fn string_map_keeps_key_order_and_drops_non_strings() {
        let got = map_doc("[env]\nB = \"2\"\nA = \"1\"\nC = 3\n");
        // BTreeMap order — the same order iterating a `toml::Table` gave.
        assert_eq!(
            got,
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
    }

    #[test]
    fn string_map_absent_or_wrong_typed_is_empty() {
        assert!(map_doc("").is_empty());
        assert!(map_doc("env = \"nope\"\n").is_empty());
        assert!(map_doc("env = [1, 2]\n").is_empty());
    }

    #[derive(Debug, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct LossyDoc {
        #[serde(deserialize_with = "opt_number_lossy")]
        hours: Option<f64>,
    }

    fn lossy(s: &str) -> Option<f64> {
        toml::from_str::<LossyDoc>(s)
            .expect("must never fail to parse")
            .hours
    }

    #[test]
    fn opt_number_lossy_accepts_integers_unlike_the_strict_helper() {
        // Deliberately the OPPOSITE quirk from `opt_float_strict`: these
        // readers wrote `as_float().or_else(|| as_integer()…)`.
        assert_eq!(lossy("hours = 24\n"), Some(24.0));
        assert_eq!(lossy("hours = 1.5\n"), Some(1.5));
        assert_eq!(lossy("hours = \"24\"\n"), None);
        assert_eq!(lossy("hours = true\n"), None);
        assert_eq!(lossy(""), None);
    }

    #[test]
    fn works_through_json_too() {
        // AgentConfig round-trips through serde_json for the dashboard, so the
        // helpers must not be toml-specific.
        let d: Doc = serde_json::from_str(r#"{"section":{"name":5,"list":["a",2]}}"#).unwrap();
        assert_eq!(d.section.name, None);
        assert_eq!(d.section.list, vec!["a".to_string()]);
    }
}
