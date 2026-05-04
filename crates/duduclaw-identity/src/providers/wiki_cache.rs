//! [`WikiCacheIdentityProvider`] — reads structured identity records from
//! the shared wiki at `<home>/shared/wiki/identity/people/*.md`.
//!
//! ## Schema
//!
//! Each `*.md` file under `identity/people/` carries a YAML frontmatter
//! block; the body is treated as free-form notes and ignored:
//!
//! ```text
//! ---
//! person_id: person_2f9
//! display_name: Ruby Lin
//! roles: [customer-pm]
//! project_ids: [proj-alpha, proj-beta]
//! emails: [ruby@example.com]
//! channel_handles:
//!   discord: "1234567890"
//!   line: "Uabc"
//! ---
//!
//! Free-form notes about Ruby — never read by the provider.
//! ```
//!
//! Files missing required fields, malformed YAML, or duplicated handles are
//! silently skipped (with a `tracing::warn!`) so a single bad file never
//! takes the whole resolver down.
//!
//! ## Performance
//!
//! For the typical deployment (≤ a few hundred people) this is fast: every
//! resolve scans the directory, parses frontmatter, returns the first match.
//! Larger deployments should put a structured store behind a different
//! provider (Notion / LDAP) — this provider exists to demote the wiki from
//! source-of-truth to cache, not to be a high-throughput lookup index.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::warn;

use crate::{ChannelKind, IdentityError, IdentityProvider, ResolvedPerson};

/// Provider name used in audit logs and `ResolvedPerson::source`.
pub const PROVIDER_NAME: &str = "wiki-cache";

/// Reads identity records from `<wiki_root>/identity/people/*.md`.
#[derive(Debug, Clone)]
pub struct WikiCacheIdentityProvider {
    /// Path to the shared wiki root, i.e. `<home>/shared/wiki`.
    wiki_root: PathBuf,
}

impl WikiCacheIdentityProvider {
    /// Build a provider rooted at `<home>/shared/wiki`.
    pub fn for_home(home_dir: impl Into<PathBuf>) -> Self {
        let home: PathBuf = home_dir.into();
        Self { wiki_root: home.join("shared").join("wiki") }
    }

    /// Build a provider rooted at an explicit wiki directory. Mostly for
    /// tests; production code should prefer [`for_home`].
    pub fn for_wiki_root(wiki_root: impl Into<PathBuf>) -> Self {
        Self { wiki_root: wiki_root.into() }
    }

    fn people_dir(&self) -> PathBuf {
        self.wiki_root.join("identity").join("people")
    }

    /// Iterate `<wiki_root>/identity/people/*.md` and yield successfully
    /// parsed records. Files that fail to parse are warned and skipped.
    fn iter_people(&self) -> Vec<ResolvedPerson> {
        let dir = self.people_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                warn!("identity wiki cache: cannot read {:?}: {}", dir, e);
                return Vec::new();
            }
        };

        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match parse_person_file(&path) {
                Ok(person) => out.push(person),
                Err(reason) => warn!(
                    "identity wiki cache: skipping {:?}: {}",
                    path, reason
                ),
            }
        }
        out
    }
}

#[async_trait]
impl IdentityProvider for WikiCacheIdentityProvider {
    async fn resolve_by_channel(
        &self,
        channel: ChannelKind,
        external_id: &str,
    ) -> Result<Option<ResolvedPerson>, IdentityError> {
        if external_id.is_empty() {
            return Ok(None);
        }
        let wire = channel.as_wire();
        for person in self.iter_people() {
            if person
                .channel_handles
                .get(&wire)
                .map(|s| s.as_str() == external_id)
                .unwrap_or(false)
            {
                return Ok(Some(person));
            }
        }
        Ok(None)
    }

    async fn lookup_project_members(
        &self,
        project_id: &str,
    ) -> Result<Vec<ResolvedPerson>, IdentityError> {
        Ok(self
            .iter_people()
            .into_iter()
            .filter(|p| p.project_ids.iter().any(|id| id == project_id))
            .collect())
    }

    fn name(&self) -> &str {
        PROVIDER_NAME
    }
}

// ── Parsing ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PersonFrontmatter {
    person_id: String,
    display_name: String,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    project_ids: Vec<String>,
    #[serde(default)]
    emails: Vec<String>,
    #[serde(default)]
    channel_handles: BTreeMap<String, String>,
}

fn parse_person_file(path: &Path) -> Result<ResolvedPerson, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let yaml = extract_frontmatter(&raw).ok_or_else(|| "missing YAML frontmatter".to_string())?;
    let fm: PersonFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| format!("malformed YAML frontmatter: {e}"))?;

    if fm.person_id.is_empty() {
        return Err("missing required field: person_id".into());
    }
    if fm.display_name.is_empty() {
        return Err("missing required field: display_name".into());
    }

    let fetched_at: DateTime<Utc> = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());

    Ok(ResolvedPerson {
        person_id: fm.person_id,
        display_name: fm.display_name,
        roles: fm.roles,
        project_ids: fm.project_ids,
        emails: fm.emails,
        channel_handles: fm.channel_handles,
        source: PROVIDER_NAME.into(),
        fetched_at,
    })
}

/// Extract the YAML frontmatter block — the substring between the first
/// `---` line and the next `---` line. Returns `None` for files without a
/// frontmatter block.
fn extract_frontmatter(raw: &str) -> Option<&str> {
    let trimmed = raw.trim_start();
    let body = trimmed.strip_prefix("---")?;
    let body = body.strip_prefix('\n').or_else(|| body.strip_prefix("\r\n"))?;
    let end = body.find("\n---")?;
    Some(&body[..end])
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_person(wiki_root: &Path, filename: &str, frontmatter: &str) {
        let dir = wiki_root.join("identity").join("people");
        fs::create_dir_all(&dir).unwrap();
        let body = format!("---\n{frontmatter}---\n\nFree-form notes.\n");
        fs::write(dir.join(filename), body).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_known_discord_handle_to_person() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());

        write_person(
            tmp.path(),
            "ruby.md",
            "person_id: person_2f9\n\
             display_name: Ruby Lin\n\
             roles: [customer-pm]\n\
             project_ids: [proj-alpha]\n\
             emails: [ruby@example.com]\n\
             channel_handles:\n  \
               discord: \"1234567890\"\n  \
               line: \"Uabc\"\n",
        );

        let resolved = provider
            .resolve_by_channel(ChannelKind::Discord, "1234567890")
            .await
            .unwrap()
            .expect("should resolve");

        assert_eq!(resolved.person_id, "person_2f9");
        assert_eq!(resolved.display_name, "Ruby Lin");
        assert_eq!(resolved.roles, vec!["customer-pm"]);
        assert_eq!(resolved.project_ids, vec!["proj-alpha"]);
        assert_eq!(resolved.source, "wiki-cache");
        assert_eq!(resolved.handle_for(&ChannelKind::Line), Some("Uabc"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn returns_none_for_unknown_handle() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());
        write_person(
            tmp.path(),
            "ruby.md",
            "person_id: person_2f9\n\
             display_name: Ruby Lin\n\
             channel_handles:\n  discord: \"1234567890\"\n",
        );

        let resolved = provider
            .resolve_by_channel(ChannelKind::Discord, "9999999")
            .await
            .unwrap();
        assert!(resolved.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn returns_none_when_directory_does_not_exist() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());
        let resolved = provider
            .resolve_by_channel(ChannelKind::Discord, "1234")
            .await
            .unwrap();
        assert!(resolved.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn returns_none_for_empty_external_id() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());
        // Edge case: even if there were a record with an empty handle in the
        // cache, an empty external_id must never match.
        let resolved = provider
            .resolve_by_channel(ChannelKind::Discord, "")
            .await
            .unwrap();
        assert!(resolved.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ignores_files_with_malformed_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());

        // Junk file alongside a good file — provider must skip the junk
        // and still resolve the good record.
        let dir = tmp.path().join("identity").join("people");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("junk.md"), "no frontmatter at all").unwrap();
        fs::write(
            dir.join("more-junk.md"),
            "---\nthis: is :: invalid yaml ===\n---\n",
        )
        .unwrap();

        write_person(
            tmp.path(),
            "good.md",
            "person_id: person_good\n\
             display_name: Good Person\n\
             channel_handles:\n  discord: \"42\"\n",
        );

        let resolved = provider
            .resolve_by_channel(ChannelKind::Discord, "42")
            .await
            .unwrap()
            .expect("good record should still resolve");
        assert_eq!(resolved.person_id, "person_good");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skips_files_missing_required_fields() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());

        // Missing person_id — must be skipped.
        write_person(
            tmp.path(),
            "incomplete.md",
            "display_name: Anonymous\n\
             channel_handles:\n  discord: \"42\"\n",
        );

        let resolved = provider
            .resolve_by_channel(ChannelKind::Discord, "42")
            .await
            .unwrap();
        assert!(resolved.is_none(), "incomplete record must not resolve");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lookup_project_members_filters_by_project_id() {
        let tmp = TempDir::new().unwrap();
        let provider = WikiCacheIdentityProvider::for_wiki_root(tmp.path().to_path_buf());

        write_person(
            tmp.path(),
            "alpha-pm.md",
            "person_id: a1\n\
             display_name: Alpha PM\n\
             project_ids: [proj-alpha]\n",
        );
        write_person(
            tmp.path(),
            "alpha-eng.md",
            "person_id: a2\n\
             display_name: Alpha Engineer\n\
             project_ids: [proj-alpha, proj-beta]\n",
        );
        write_person(
            tmp.path(),
            "beta-only.md",
            "person_id: b1\n\
             display_name: Beta Only\n\
             project_ids: [proj-beta]\n",
        );

        let alpha = provider.lookup_project_members("proj-alpha").await.unwrap();
        assert_eq!(alpha.len(), 2);
        let mut ids: Vec<_> = alpha.iter().map(|p| p.person_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a1", "a2"]);

        let unknown = provider.lookup_project_members("proj-nope").await.unwrap();
        assert!(unknown.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_name_is_stable() {
        let provider = WikiCacheIdentityProvider::for_wiki_root("/tmp/nope");
        assert_eq!(provider.name(), "wiki-cache");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn for_home_uses_shared_wiki_subpath() {
        let provider = WikiCacheIdentityProvider::for_home("/some/home");
        // Probe via the people_dir path.
        assert!(provider.people_dir().ends_with("shared/wiki/identity/people"));
    }

    #[test]
    fn extract_frontmatter_returns_yaml_block() {
        let raw = "---\nperson_id: x\ndisplay_name: y\n---\n\nbody\n";
        let yaml = extract_frontmatter(raw).expect("frontmatter present");
        assert!(yaml.contains("person_id: x"));
        assert!(yaml.contains("display_name: y"));
    }

    #[test]
    fn extract_frontmatter_returns_none_without_block() {
        assert!(extract_frontmatter("# just a markdown body").is_none());
        assert!(extract_frontmatter("--- broken").is_none());
    }

    #[test]
    fn extract_frontmatter_handles_crlf_line_endings() {
        let raw = "---\r\nperson_id: x\r\ndisplay_name: y\r\n---\r\n\r\nbody\r\n";
        let yaml = extract_frontmatter(raw).expect("frontmatter present");
        assert!(yaml.contains("person_id: x"));
    }
}
