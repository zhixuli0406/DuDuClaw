//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Serialize one memory entry for the dashboard (`memory.browse` /
/// `memory.search`). Shared so both surfaces stay field-identical.
///
/// The cognitive fields (`layer` / `source_event` / `importance` /
/// `access_count`) are what the memory page's topic grouping runs on: the
/// dashboard classifies an entry deterministically from its origin before it
/// falls back to content keywords, so a `footprint_distill` entry never lands
/// in the same bucket as a user-stated preference.
/// The decay figures ride along on every row (`retrievability` / `stability_days`)
/// so the memory page can show a freshness state per entry without a second
/// round-trip. Both are derived from `duduclaw_memory::engine`'s own functions —
/// the ones `decay::run_decay` scores against — so what the dashboard shows and
/// what the archival job acts on can never drift apart.
pub(crate) fn memory_entry_row(
    e: &duduclaw_core::types::MemoryEntry,
    w: &duduclaw_memory::engine::RetrievalWeights,
    now: DateTime<Utc>,
) -> Value {
    let (retrievability, stability_days) = memory_decay_figures(e, w, now);
    json!({
        "id": e.id,
        "agent_id": e.agent_id,
        "content": e.content,
        "timestamp": e.timestamp.to_rfc3339(),
        "tags": e.tags,
        "layer": e.layer.as_str(),
        "source_event": e.source_event,
        "importance": e.importance,
        "access_count": e.access_count,
        "last_accessed": e.last_accessed.map(|t| t.to_rfc3339()),
        "retrievability": round_to(retrievability, 4),
        "stability_days": round_to(stability_days, 2),
    })
}

/// `(retrievability, stability_days)` for one entry at `now`.
///
/// The elapsed-time anchor is `last_accessed`, falling back to `timestamp` for a
/// never-recalled entry — byte-identical to the anchor `decay::run_decay` picks,
/// which is the whole point of factoring it out here.
pub(crate) fn memory_decay_figures(
    e: &duduclaw_core::types::MemoryEntry,
    w: &duduclaw_memory::engine::RetrievalWeights,
    now: DateTime<Utc>,
) -> (f64, f64) {
    let anchor = e.last_accessed.unwrap_or(e.timestamp);
    let days = (now - anchor).num_seconds().max(0) as f64 / 86_400.0;
    (
        duduclaw_memory::engine::ebbinghaus_retrievability(days, e.access_count, e.importance, w),
        duduclaw_memory::engine::ebbinghaus_stability_days(e.access_count, e.importance, w),
    )
}

/// Round to `decimals` places for the wire. Keeps the JSON readable and stops a
/// float artefact (`0.30000000000000004`) from reaching a chart axis.
pub(crate) fn round_to(value: f64, decimals: u32) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

/// Freshness bands used by the memory page's decay visualisation, ordered
/// freshest → faintest. `(key, lower_bound_inclusive)`; the last band is the
/// remainder down to 0. The keys are stable wire values — the dashboard maps
/// them to plain-language labels, never the other way round.
pub(crate) const MEMORY_FRESHNESS_BANDS: &[(&str, f64)] = &[
    ("fresh", 0.7),
    ("stable", 0.4),
    ("fading", 0.15),
    ("archiving", 0.0),
];

/// Classify a retrievability value into one of [`MEMORY_FRESHNESS_BANDS`].
pub(crate) fn memory_freshness_band(retrievability: f64) -> &'static str {
    MEMORY_FRESHNESS_BANDS
        .iter()
        .find(|(_, lower)| retrievability >= *lower)
        .map(|(key, _)| *key)
        .unwrap_or("archiving")
}

/// Render the freshness histogram in band order, including bands with a zero
/// count — a distribution chart with a silently-missing category reads as if
/// that state does not exist.
pub(crate) fn memory_freshness_bucket_rows(counts: &HashMap<&'static str, u64>) -> Vec<Value> {
    MEMORY_FRESHNESS_BANDS
        .iter()
        .map(|(key, lower)| {
            json!({
                "key": key,
                "count": counts.get(key).copied().unwrap_or(0),
                "min_retrievability": lower,
            })
        })
        .collect()
}

/// Upper bound on rows `memory.decay_overview` scans (newest first). A dashboard
/// aggregate must not turn into an unbounded table scan; the response flags
/// `truncated` when the cap actually binds.
pub(crate) const MEMORY_DECAY_SCAN_CAP: usize = 5000;

/// D3 wiring — fold `config.toml [memory]` graph-seed knobs into
/// [`RetrievalWeights`]. Pure so it is unit-testable. Starts from engine
/// defaults; overrides only `graph_embed_seed` (bool) and
/// `graph_embed_seed_top_k` (usize, floored at 1) when present. `None` table or
/// absent keys ⇒ engine defaults (seed off, top-k 5), so ranking is unchanged.
pub(crate) fn memory_retrieval_weights_from_table(
    table: Option<&toml::Table>,
) -> duduclaw_memory::engine::RetrievalWeights {
    let mut weights = duduclaw_memory::engine::RetrievalWeights::default();
    if let Some(mem) = table
        .and_then(|t| t.get("memory"))
        .and_then(|v| v.as_table())
    {
        if let Some(seed) = mem.get("graph_embed_seed").and_then(|v| v.as_bool()) {
            weights.graph_embed_seed = seed;
        }
        if let Some(k) = mem
            .get("graph_embed_seed_top_k")
            .and_then(|v| v.as_integer())
        {
            weights.graph_embed_seed_top_k = k.max(1) as usize;
        }
    }
    weights
}
