//! WP2.6 §5 — contextual recommendation signal: attachment file-extension gaps.
//!
//! When a channel conversation carries a file whose type an agent has no skill
//! for (a `.psd` reaching an agent with no design skill, a `.dwg` with no CAD
//! skill, …), that is a concrete "you should install a skill" signal — the
//! translation of VS Code's "open an unknown file type → recommend an
//! extension" behaviour into DuDuClaw's skill marketplace.
//!
//! This is the **P0-minimal** version: cheap recording at the single attachment
//! chokepoint (`extension → capability keyword`), persisted to
//! `<home>/skill_ext_gaps.jsonl`, and queryable via the existing recommendation
//! exit (the `skill_gaps` MCP tool cross-references recorded extensions against
//! an agent's installed skills). The full daily-cron aggregation → federated
//! search → recommendation-card pipeline is deferred to P1.
//!
//! Non-goals / boundaries: this does **not** overlap `office_docs.rs` (which
//! deterministically boosts an *already-installed* office skill for docx/xlsx/…
//! attachments). This module records a gap only for extensions that map to a
//! capability the agent is *missing* — the two are complementary, not
//! duplicative.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Extension → capability keyword. The keyword is what we later feed to
/// federated `skill_search` and match against an agent's installed skill
/// names/tags. Deliberately small and high-signal — file types a general
/// office/coding agent typically can't handle without a dedicated skill.
///
/// Office document types (docx/xlsx/pptx/pdf/csv) are intentionally **excluded**
/// — those are handled by `office_docs.rs` skill-boosting, not gap recording.
const EXT_CAPABILITY: &[(&str, &str)] = &[
    ("psd", "photoshop"),
    ("ai", "illustrator"),
    ("sketch", "sketch design"),
    ("fig", "figma design"),
    ("xd", "adobe xd"),
    ("dwg", "cad autocad"),
    ("dxf", "cad drawing"),
    ("step", "cad 3d model"),
    ("stp", "cad 3d model"),
    ("stl", "3d printing model"),
    ("obj", "3d model"),
    ("blend", "blender 3d"),
    ("indd", "indesign layout"),
    ("epub", "ebook epub"),
    ("mobi", "ebook"),
    ("dcm", "dicom medical imaging"),
    ("sav", "spss statistics"),
    ("dta", "stata statistics"),
    ("rdata", "r statistics"),
    ("parquet", "parquet data"),
    ("ipynb", "jupyter notebook"),
    ("kml", "geospatial gis"),
    ("shp", "geospatial gis"),
    ("geojson", "geospatial gis"),
    ("srt", "subtitle transcription"),
    ("vtt", "subtitle transcription"),
];

/// Look up the capability keyword for a file extension (lowercased, no dot).
/// `None` ⇒ not a tracked gap-signalling type.
pub fn ext_to_capability(ext: &str) -> Option<&'static str> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    EXT_CAPABILITY
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, cap)| *cap)
}

/// Extract a lowercase extension from a filename (`report.final.PSD` → `psd`).
pub fn extension_of(filename: &str) -> Option<String> {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let ext = base.rsplit_once('.')?.1;
    if ext.is_empty() || ext.len() > 12 || ext.contains(char::is_whitespace) {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// One recorded extension-gap observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtGapRecord {
    pub agent_id: String,
    pub ext: String,
    pub capability: String,
    pub filename: String,
    pub at: String,
}

/// A capability gap aggregated across recorded observations, ready for the
/// recommendation exit.
#[derive(Debug, Clone, Serialize)]
pub struct AggregatedExtGap {
    pub capability: String,
    pub exts: Vec<String>,
    pub count: usize,
    pub last_seen: String,
    pub sample_filename: String,
}

fn log_path(home_dir: &Path) -> PathBuf {
    home_dir.join("skill_ext_gaps.jsonl")
}

/// Record an attachment observation as a potential capability gap, iff the
/// extension is one of the tracked gap-signalling types. No-op for untracked
/// extensions (office docs, images, plain text, …). Cross-process safe: the
/// append holds an advisory file lock.
///
/// This is the cheap recording half — it does NOT check whether the agent
/// already has a matching skill (that refinement happens at query time in
/// [`aggregate_gaps_for_agent`], keeping this chokepoint free of skill-registry
/// coupling). Returns the capability keyword when something was recorded.
pub fn record_attachment(home_dir: &Path, agent_id: &str, filename: &str) -> Option<String> {
    let ext = extension_of(filename)?;
    let capability = ext_to_capability(&ext)?;
    let record = ExtGapRecord {
        agent_id: agent_id.to_string(),
        ext,
        capability: capability.to_string(),
        filename: duduclaw_core::truncate_chars(filename, 200),
        at: Utc::now().to_rfc3339(),
    };
    let line = match serde_json::to_string(&record) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("skill_ext_gap serialize: {e}");
            return None;
        }
    };
    let path = log_path(home_dir);
    let write = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "{line}")
    });
    match write {
        Ok(_) => Some(capability.to_string()),
        Err(e) => {
            tracing::warn!("skill_ext_gap append: {e}");
            None
        }
    }
}

/// Read all recorded gap observations (best-effort; malformed lines skipped).
pub fn read_all(home_dir: &Path) -> Vec<ExtGapRecord> {
    let Ok(content) = std::fs::read_to_string(log_path(home_dir)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ExtGapRecord>(l).ok())
        .collect()
}

/// Aggregate recorded gaps for one agent, **excluding** capabilities the agent
/// already covers (a keyword term appearing in an installed skill name/tag).
/// `installed_terms` is the agent's installed skill names + tags, lowercased.
/// Sorted by occurrence count desc.
pub fn aggregate_gaps_for_agent(
    records: &[ExtGapRecord],
    agent_id: &str,
    installed_terms: &[String],
) -> Vec<AggregatedExtGap> {
    use std::collections::BTreeMap;
    let mut by_cap: BTreeMap<String, AggregatedExtGap> = BTreeMap::new();
    for r in records.iter().filter(|r| r.agent_id == agent_id) {
        // Skip capabilities the agent already covers (any keyword token matches
        // an installed term) — that's not a gap.
        let covered = r.capability.split_whitespace().any(|kw| {
            let kw = kw.to_ascii_lowercase();
            installed_terms.iter().any(|t| t.contains(&kw))
        });
        if covered {
            continue;
        }
        let agg = by_cap
            .entry(r.capability.clone())
            .or_insert_with(|| AggregatedExtGap {
                capability: r.capability.clone(),
                exts: Vec::new(),
                count: 0,
                last_seen: r.at.clone(),
                sample_filename: r.filename.clone(),
            });
        agg.count += 1;
        if !agg.exts.contains(&r.ext) {
            agg.exts.push(r.ext.clone());
        }
        if r.at > agg.last_seen {
            agg.last_seen = r.at.clone();
            agg.sample_filename = r.filename.clone();
        }
    }
    let mut out: Vec<AggregatedExtGap> = by_cap.into_values().collect();
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.capability.cmp(&b.capability))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_and_capability_mapping() {
        assert_eq!(extension_of("design.PSD").as_deref(), Some("psd"));
        assert_eq!(extension_of("a/b/c.final.dwg").as_deref(), Some("dwg"));
        assert_eq!(extension_of("noext"), None);
        assert_eq!(ext_to_capability("psd"), Some("photoshop"));
        assert_eq!(ext_to_capability(".DWG"), Some("cad autocad"));
        // Office docs are NOT gap-signalling here (office_docs.rs owns them).
        assert_eq!(ext_to_capability("docx"), None);
        assert_eq!(ext_to_capability("xlsx"), None);
        assert_eq!(ext_to_capability("txt"), None);
    }

    #[test]
    fn record_and_aggregate_roundtrip() {
        let home = std::env::temp_dir().join(format!("dc-extgap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();

        assert_eq!(
            record_attachment(&home, "designer", "hero.psd").as_deref(),
            Some("photoshop")
        );
        record_attachment(&home, "designer", "logo.psd");
        record_attachment(&home, "designer", "plan.dwg");
        // Untracked type ⇒ nothing recorded.
        assert_eq!(record_attachment(&home, "designer", "notes.txt"), None);
        // Different agent — must not leak across agents.
        record_attachment(&home, "other", "x.psd");

        let records = read_all(&home);
        assert_eq!(records.len(), 4);

        // Agent with no matching skills: both gaps surface, psd ranks first (2×).
        let gaps = aggregate_gaps_for_agent(&records, "designer", &[]);
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].capability, "photoshop");
        assert_eq!(gaps[0].count, 2);

        // Agent that already has a "photoshop editor" skill ⇒ psd is covered,
        // only the cad gap remains.
        let installed = vec!["photoshop editor".to_string()];
        let gaps = aggregate_gaps_for_agent(&records, "designer", &installed);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].capability, "cad autocad");

        let _ = std::fs::remove_dir_all(&home);
    }
}
