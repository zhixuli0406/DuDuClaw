//! Identity rule — redact the display names of the people the deployment
//! already knows about, without an operator retyping them as keywords.
//!
//! The name list comes from the shared wiki's identity directory
//! (`<home>/shared/wiki/identity/people/*.md`), parsed by
//! [`WikiCacheIdentityProvider`] — the same frontmatter parser the identity
//! resolver uses, so a person file is written once and both subsystems agree
//! on it. Only `display_name` is taken: emails are already covered by the
//! regex profiles and the people schema has no alias field.
//!
//! Matching semantics are identical to a case-insensitive
//! [`crate::rules::KeywordRule`] (ASCII whole-word, CJK substring) — both call
//! [`match_needles`].
//!
//! ## Refresh
//!
//! The name list is a snapshot, refreshed lazily from `match_text`: when the
//! people directory's mtime differs from the snapshot's, or the snapshot is
//! older than the refresh interval (60 s in production). A new colleague or a
//! rename therefore takes effect without restarting the gateway, and a busy
//! turn does not re-read the directory for every string leaf.
//!
//! ## Fail-closed vs. fail-quiet
//!
//! - A `source` other than `"wiki"`, or a people directory that does not
//!   exist, is a **misconfiguration** → the rule fails to compile, which fails
//!   the whole `RedactionManager::open` (an operator who wrote an identity
//!   rule must never end up with it silently absent).
//! - A directory that exists with zero parseable people is a **normal**
//!   state (identity sync has not run yet) → warn once at compile time, match
//!   nothing, and pick people up as soon as they appear.

use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant, SystemTime};

use duduclaw_identity::providers::WikiCacheIdentityProvider;

use crate::error::{RedactionError, Result};
use crate::rules::keyword::match_needles;
use crate::rules::{Match, RestoreScope, Rule, RuleKind, RuleSpec};

/// The only `source` value this build understands. Compared with exact
/// equality — a substring test would accept `"wiki-but-actually-notion"`.
const SOURCE_WIKI: &str = "wiki";

/// How long a name snapshot may be reused when the directory mtime has not
/// moved. Short enough that an operator adding a person sees it take effect
/// within a minute, long enough that a chatty turn is not I/O-bound.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The cached name list plus the two facts that decide when to redo it.
#[derive(Debug)]
struct Snapshot {
    /// When the directory was last scanned.
    scanned_at: Instant,
    /// Directory mtime observed during that scan (`None` if unreadable —
    /// which then differs from any readable value and forces a re-scan).
    dir_mtime: Option<SystemTime>,
    /// Lowercased, trimmed, de-duplicated display names.
    needles: Vec<String>,
}

/// Compiled identity rule.
#[derive(Debug)]
pub struct IdentityRule {
    spec: RuleSpec,
    people_dir: PathBuf,
    refresh_interval: Duration,
    snapshot: RwLock<Snapshot>,
}

impl IdentityRule {
    /// Compile `spec` against the identity people directory, with the
    /// production refresh interval.
    pub fn compile(spec: RuleSpec, people_dir: PathBuf) -> Result<Self> {
        Self::compile_with_refresh(spec, people_dir, DEFAULT_REFRESH_INTERVAL)
    }

    /// Same, with an explicit refresh interval — tests use a zero interval to
    /// force a re-scan on every call, or a long one to prove the mtime path
    /// triggers on its own.
    pub(crate) fn compile_with_refresh(
        spec: RuleSpec,
        people_dir: PathBuf,
        refresh_interval: Duration,
    ) -> Result<Self> {
        let source = match &spec.kind {
            RuleKind::Identity { source } => source.trim().to_string(),
            other => {
                return Err(RedactionError::rule_compile(
                    &spec.id,
                    format!("expected Identity kind, got {other:?}"),
                ));
            }
        };
        // Empty ⇒ the documented default. Anything else must be exactly "wiki".
        if !source.is_empty() && source != SOURCE_WIKI {
            return Err(RedactionError::rule_compile(
                &spec.id,
                format!(
                    "unknown identity source '{source}' — only \"wiki\" is supported in this build"
                ),
            ));
        }

        if !people_dir.is_dir() {
            return Err(RedactionError::rule_compile(
                &spec.id,
                format!(
                    "identity people directory not found: {} (expected \
                     <home>/shared/wiki/identity/people)",
                    people_dir.display()
                ),
            ));
        }

        let needles = scan_needles(&people_dir);
        if needles.is_empty() {
            tracing::warn!(
                target: "duduclaw_redaction::rules::identity",
                rule_id = %spec.id,
                dir = %people_dir.display(),
                "identity rule compiled with zero people — it matches nothing \
                 until identity records appear"
            );
        }

        let snapshot = Snapshot {
            scanned_at: Instant::now(),
            dir_mtime: dir_mtime(&people_dir),
            needles,
        };

        Ok(IdentityRule {
            spec,
            people_dir,
            refresh_interval,
            snapshot: RwLock::new(snapshot),
        })
    }

    /// Number of names currently in the snapshot (diagnostics / tests).
    pub fn name_count(&self) -> usize {
        self.read_snapshot().needles.len()
    }

    /// A poisoned lock must never take the redaction path down: a panicking
    /// reader left the snapshot readable (it is plain data), so recover the
    /// guard rather than unwrapping.
    fn read_snapshot(&self) -> RwLockReadGuard<'_, Snapshot> {
        self.snapshot.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_snapshot(&self) -> RwLockWriteGuard<'_, Snapshot> {
        self.snapshot.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-scan the people directory when the snapshot is out of date.
    fn refresh_if_stale(&self) {
        {
            let snap = self.read_snapshot();
            if !is_stale(&snap, dir_mtime(&self.people_dir), self.refresh_interval) {
                return;
            }
        }
        let mut snap = self.write_snapshot();
        // Re-check under the write lock: another thread may have refreshed
        // while this one waited, and a second scan would be pure I/O waste.
        let current = dir_mtime(&self.people_dir);
        if !is_stale(&snap, current, self.refresh_interval) {
            return;
        }
        snap.needles = scan_needles(&self.people_dir);
        snap.dir_mtime = current;
        snap.scanned_at = Instant::now();
    }
}

/// Is `snap` out of date? Either the directory changed underneath it, or it
/// simply aged out (which also covers an edit that left the mtime alone).
fn is_stale(snap: &Snapshot, current_mtime: Option<SystemTime>, interval: Duration) -> bool {
    current_mtime != snap.dir_mtime || snap.scanned_at.elapsed() >= interval
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

/// Read every parseable person record and turn it into the needle list the
/// shared matcher expects: trimmed, non-empty, lowercased (the rule is
/// case-insensitive), de-duplicated and sorted so the scan order does not
/// depend on `read_dir` order.
fn scan_needles(people_dir: &Path) -> Vec<String> {
    let provider = WikiCacheIdentityProvider::for_people_dir(people_dir.to_path_buf());
    let mut names: Vec<String> = provider
        .list_people_sync()
        .into_iter()
        .filter_map(|p| {
            let name = p.display_name.trim().to_lowercase();
            if name.is_empty() { None } else { Some(name) }
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

impl Rule for IdentityRule {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn category(&self) -> &str {
        &self.spec.category
    }
    fn restore_scope(&self) -> &RestoreScope {
        &self.spec.restore_scope
    }
    fn priority(&self) -> i32 {
        self.spec.priority
    }
    fn cross_session_stable(&self) -> bool {
        self.spec.cross_session_stable
    }
    fn apply_to_system_prompt(&self) -> bool {
        self.spec.apply_to_system_prompt
    }

    fn match_text(&self, text: &str) -> Vec<Match> {
        self.refresh_if_stale();
        let snap = self.read_snapshot();
        if snap.needles.is_empty() {
            return Vec::new();
        }
        match_needles(text, &snap.needles, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn spec(source: &str) -> RuleSpec {
        RuleSpec {
            id: "people".into(),
            category: "PERSON".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Identity {
                source: source.into(),
            },
        }
    }

    fn people_dir(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join("shared/wiki/identity/people");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_person(dir: &Path, file: &str, person_id: &str, display_name: &str) {
        fs::write(
            dir.join(file),
            format!("---\nperson_id: {person_id}\ndisplay_name: {display_name}\n---\n\nnotes\n"),
        )
        .unwrap();
    }

    #[test]
    fn unknown_source_is_a_compile_error() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        let err = IdentityRule::compile(spec("notion"), dir).unwrap_err();
        assert!(matches!(err, RedactionError::RuleCompile { .. }), "{err}");
        assert!(err.to_string().contains("notion"), "{err}");
    }

    #[test]
    fn empty_source_defaults_to_wiki() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        assert!(IdentityRule::compile(spec(""), dir.clone()).is_ok());
        assert!(IdentityRule::compile(spec("  wiki  "), dir).is_ok());
    }

    #[test]
    fn missing_people_directory_is_a_compile_error() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("shared/wiki/identity/people");
        let err = IdentityRule::compile(spec("wiki"), missing).unwrap_err();
        assert!(matches!(err, RedactionError::RuleCompile { .. }), "{err}");

        // A *file* where the directory should be is the same misconfiguration.
        let as_file = tmp.path().join("people-file");
        fs::write(&as_file, b"not a directory").unwrap();
        assert!(IdentityRule::compile(spec("wiki"), as_file).is_err());
    }

    #[test]
    fn wrong_rule_kind_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        let mut s = spec("wiki");
        s.kind = RuleKind::Keyword {
            values: vec!["x".into()],
            case_sensitive: false,
        };
        assert!(IdentityRule::compile(s, dir).is_err());
    }

    #[test]
    fn empty_directory_matches_nothing_then_picks_up_a_new_person() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        // Zero-interval refresh ⇒ every match_text re-scans, which isolates
        // the "time elapsed" half of the staleness rule from the mtime half.
        let rule =
            IdentityRule::compile_with_refresh(spec("wiki"), dir.clone(), Duration::ZERO).unwrap();
        assert_eq!(rule.name_count(), 0);
        assert!(rule.match_text("王小明 打電話來").is_empty());

        write_person(&dir, "ming.md", "p_ming", "王小明");

        let hits = rule.match_text("王小明 打電話來");
        assert_eq!(hits.len(), 1, "new person must be picked up");
        assert_eq!(hits[0].original, "王小明");
    }

    #[test]
    fn directory_mtime_change_alone_triggers_a_rescan() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        // A refresh interval far longer than the test: only the mtime check
        // can make this snapshot stale.
        let rule = IdentityRule::compile_with_refresh(
            spec("wiki"),
            dir.clone(),
            Duration::from_secs(3600),
        )
        .unwrap();
        assert!(rule.match_text("Ruby Lin called").is_empty());

        // Make sure the new directory mtime is distinguishable from the one
        // captured at compile time.
        std::thread::sleep(Duration::from_millis(20));
        write_person(&dir, "ruby.md", "p_ruby", "Ruby Lin");

        let hits = rule.match_text("Ruby Lin called");
        assert_eq!(hits.len(), 1, "mtime change must force a re-scan");
        assert_eq!(hits[0].original, "Ruby Lin");
    }

    #[test]
    fn ascii_names_are_whole_word_and_cjk_names_are_substrings() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        write_person(&dir, "ruby.md", "p_ruby", "Ruby Lin");
        write_person(&dir, "ming.md", "p_ming", "王小明");
        let rule = IdentityRule::compile(spec("wiki"), dir).unwrap();

        // ASCII: whole word only.
        let hits = rule.match_text("Ruby Lin signed off");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].original, "Ruby Lin");
        assert!(
            rule.match_text("Ruby Linden signed off").is_empty(),
            "ASCII name must not fire inside a longer word"
        );

        // Case-insensitive, original casing preserved in the match.
        let hits = rule.match_text("RUBY LIN signed off");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].original, "RUBY LIN");

        // CJK: substring, no boundary requirement.
        let hits = rule.match_text("客戶王小明的訂單");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].original, "王小明");
    }

    #[test]
    fn duplicate_display_names_collapse_to_one_needle() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        write_person(&dir, "a.md", "p_a", "Ruby Lin");
        write_person(&dir, "b.md", "p_b", "  ruby lin  ");
        let rule = IdentityRule::compile(spec("wiki"), dir).unwrap();

        assert_eq!(rule.name_count(), 1, "same name twice must not double-scan");
        assert_eq!(rule.match_text("Ruby Lin here").len(), 1);
    }

    #[test]
    fn unparseable_person_files_are_skipped_not_fatal() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        fs::write(dir.join("junk.md"), "no frontmatter at all").unwrap();
        write_person(&dir, "good.md", "p_good", "Good Person");
        let rule = IdentityRule::compile(spec("wiki"), dir).unwrap();

        assert_eq!(rule.name_count(), 1);
        assert_eq!(rule.match_text("Good Person waited").len(), 1);
    }

    #[test]
    fn rule_metadata_comes_from_the_spec() {
        let tmp = TempDir::new().unwrap();
        let dir = people_dir(&tmp);
        let rule = IdentityRule::compile(spec("wiki"), dir).unwrap();
        assert_eq!(rule.id(), "people");
        assert_eq!(rule.category(), "PERSON");
        assert_eq!(rule.priority(), 70);
        assert!(!rule.cross_session_stable());
        assert!(!rule.apply_to_system_prompt());
        assert_eq!(rule.restore_scope(), &RestoreScope::Owner);
    }
}
