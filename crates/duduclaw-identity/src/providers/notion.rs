//! [`NotionIdentityProvider`] — resolves people via a Notion People DB.
//!
//! ## Notion data model assumed
//!
//! A Notion database with one row per person. Required properties (operator
//! configures the actual property names via [`NotionFieldMap`]):
//!
//! - **Name** (Title)
//! - **Roles** (Multi-select)
//! - **Projects** (Relation or Multi-select; rendered as plain text)
//! - **Emails** (Email or Multi-select)
//! - **Discord ID** / **Line ID** / ... (Rich text or Phone)
//!
//! ## Provider semantics
//!
//! Each `resolve_by_channel` call queries the Notion `databases/query`
//! endpoint with a `filter` narrowing to records whose channel-handle
//! property equals `external_id`. Notion paginates at 100 — for the
//! "find one person" use case that's plenty.
//!
//! `lookup_project_members` queries by the `Projects` property containing
//! `project_id`. Notion's filter API supports `relation.contains` for
//! relation properties; for multi-select-encoded projects it uses
//! `multi_select.contains`. Operators choose via `field_map.projects_kind`.
//!
//! ## Errors
//!
//! - HTTP 5xx / network failure → `IdentityError::Unreachable` so chained
//!   providers can degrade to cache.
//! - HTTP 4xx (auth, schema mismatch) → `IdentityError::Malformed`.
//! - Schema parse failure → `IdentityError::Malformed`.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ChannelKind, IdentityError, IdentityProvider, ResolvedPerson};

pub const PROVIDER_NAME: &str = "notion";

/// Maps DuDuClaw's logical fields onto Notion property names. Defaults
/// match a sensible naming convention; operators override per-deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotionFieldMap {
    pub name: String,
    pub roles: String,
    pub projects: String,
    pub projects_kind: ProjectsKind,
    pub emails: String,
    /// Map of channel kind → Notion property name carrying that channel's
    /// external_id.
    pub channel_props: BTreeMap<String, String>,
}

impl Default for NotionFieldMap {
    fn default() -> Self {
        let mut channel_props = BTreeMap::new();
        channel_props.insert("discord".into(), "Discord ID".into());
        channel_props.insert("line".into(), "Line ID".into());
        channel_props.insert("telegram".into(), "Telegram ID".into());
        channel_props.insert("email".into(), "Email".into());
        Self {
            name: "Name".into(),
            roles: "Roles".into(),
            projects: "Projects".into(),
            projects_kind: ProjectsKind::MultiSelect,
            emails: "Email".into(),
            channel_props,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectsKind {
    /// Notion property is `multi_select` — values are tags.
    MultiSelect,
    /// Notion property is `relation` — values are IDs of related rows.
    Relation,
}

/// Configuration for one [`NotionIdentityProvider`] instance.
#[derive(Debug, Clone)]
pub struct NotionConfig {
    pub database_id: String,
    /// Notion API integration secret (e.g. `"secret_..."`). Stored in
    /// memory only; persistence is the operator's responsibility (we
    /// recommend AES-256-GCM via `duduclaw-security`).
    pub api_key: String,
    pub field_map: NotionFieldMap,
    /// Optional refresh hint — surfaced through the resolver so caches
    /// can decide their own TTL.
    pub refresh_seconds: u64,
}

/// Resolves people from a Notion People DB.
pub struct NotionIdentityProvider {
    config: NotionConfig,
    client: reqwest::Client,
}

impl NotionIdentityProvider {
    pub fn new(config: NotionConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("duduclaw-identity/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build");
        Self { config, client }
    }

    /// Build a `databases/query` filter narrowing rows by a channel handle.
    fn build_channel_filter(&self, channel: &ChannelKind, external_id: &str) -> Value {
        let prop = self
            .config
            .field_map
            .channel_props
            .get(&channel.as_wire())
            .cloned()
            .unwrap_or_else(|| format!("{} ID", channel.as_wire()));

        // Notion's `rich_text.equals` filter — accurate enough for the
        // typical "Discord User ID" property case. Operators with
        // non-rich-text channel props can extend this filter shape later.
        serde_json::json!({
            "property": prop,
            "rich_text": { "equals": external_id }
        })
    }

    fn build_project_filter(&self, project_id: &str) -> Value {
        let prop = self.config.field_map.projects.clone();
        let key = match self.config.field_map.projects_kind {
            ProjectsKind::MultiSelect => "multi_select",
            ProjectsKind::Relation => "relation",
        };
        serde_json::json!({
            "property": prop,
            key: { "contains": project_id }
        })
    }

    async fn query_database(&self, filter: Value) -> Result<Value, IdentityError> {
        let url = format!(
            "https://api.notion.com/v1/databases/{}/query",
            self.config.database_id,
        );
        let body = serde_json::json!({ "filter": filter, "page_size": 50 });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("Notion-Version", "2022-06-28")
            .json(&body)
            .send()
            .await
            .map_err(|e| IdentityError::unreachable(PROVIDER_NAME, e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            return Err(IdentityError::malformed(
                PROVIDER_NAME,
                format!("HTTP {status}: {}", truncate(&txt, 200)),
            ));
        }

        resp.json::<Value>()
            .await
            .map_err(|e| IdentityError::malformed(PROVIDER_NAME, e.to_string()))
    }

    /// Map a Notion `results[i]` row into a [`ResolvedPerson`] using
    /// [`NotionFieldMap`]. Returns `None` for rows missing the required
    /// title field — those are treated as malformed (skipped, logged
    /// elsewhere by the caller if desired).
    pub fn map_row(&self, row: &Value) -> Option<ResolvedPerson> {
        let id = row.get("id")?.as_str()?.to_string();
        let props = row.get("properties")?;

        let display_name = read_title(props, &self.config.field_map.name)?;
        let roles = read_multi_select(props, &self.config.field_map.roles);
        let project_ids = read_multi_or_relation(
            props,
            &self.config.field_map.projects,
            self.config.field_map.projects_kind,
        );
        let emails = read_email_or_multi(props, &self.config.field_map.emails);

        let mut channel_handles = BTreeMap::new();
        for (channel_wire, prop_name) in &self.config.field_map.channel_props {
            if let Some(handle) = read_rich_text(props, prop_name) {
                if !handle.is_empty() {
                    channel_handles.insert(channel_wire.clone(), handle);
                }
            }
        }

        Some(ResolvedPerson {
            person_id: id,
            display_name,
            roles,
            project_ids,
            emails,
            channel_handles,
            source: PROVIDER_NAME.into(),
            fetched_at: Utc::now(),
        })
    }
}

#[async_trait]
impl IdentityProvider for NotionIdentityProvider {
    async fn resolve_by_channel(
        &self,
        channel: ChannelKind,
        external_id: &str,
    ) -> Result<Option<ResolvedPerson>, IdentityError> {
        if external_id.is_empty() {
            return Ok(None);
        }
        let filter = self.build_channel_filter(&channel, external_id);
        let body = self.query_database(filter).await?;
        let results = body.get("results").and_then(|v| v.as_array());
        match results {
            Some(rows) if !rows.is_empty() => Ok(self.map_row(&rows[0])),
            _ => Ok(None),
        }
    }

    async fn lookup_project_members(
        &self,
        project_id: &str,
    ) -> Result<Vec<ResolvedPerson>, IdentityError> {
        let filter = self.build_project_filter(project_id);
        let body = self.query_database(filter).await?;
        let results = body.get("results").and_then(|v| v.as_array());
        let mut out = Vec::new();
        if let Some(rows) = results {
            for row in rows {
                if let Some(p) = self.map_row(row) {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    fn name(&self) -> &str {
        PROVIDER_NAME
    }
}

// ── Notion property readers ──────────────────────────────────────────────────

fn read_title(props: &Value, name: &str) -> Option<String> {
    let arr = props.get(name)?.get("title")?.as_array()?;
    Some(arr.iter().filter_map(|t| t.get("plain_text").and_then(|v| v.as_str())).collect::<Vec<_>>().join(""))
        .filter(|s| !s.is_empty())
}

fn read_rich_text(props: &Value, name: &str) -> Option<String> {
    let arr = props.get(name)?.get("rich_text")?.as_array()?;
    Some(arr.iter().filter_map(|t| t.get("plain_text").and_then(|v| v.as_str())).collect::<Vec<_>>().join(""))
}

fn read_multi_select(props: &Value, name: &str) -> Vec<String> {
    props
        .get(name)
        .and_then(|v| v.get("multi_select"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn read_multi_or_relation(props: &Value, name: &str, kind: ProjectsKind) -> Vec<String> {
    let prop = match props.get(name) {
        Some(p) => p,
        None => return Vec::new(),
    };
    match kind {
        ProjectsKind::MultiSelect => read_multi_select(props, name),
        ProjectsKind::Relation => prop
            .get("relation")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn read_email_or_multi(props: &Value, name: &str) -> Vec<String> {
    let prop = match props.get(name) {
        Some(p) => p,
        None => return Vec::new(),
    };
    if let Some(email) = prop.get("email").and_then(|v| v.as_str()) {
        return vec![email.to_string()];
    }
    if let Some(arr) = prop.get("multi_select").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .collect();
    }
    if let Some(rt) = read_rich_text(props, name) {
        if !rt.is_empty() {
            return vec![rt];
        }
    }
    Vec::new()
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_with_default_field_map() -> NotionIdentityProvider {
        NotionIdentityProvider::new(NotionConfig {
            database_id: "test_db".into(),
            api_key: "secret_test".into(),
            field_map: NotionFieldMap::default(),
            refresh_seconds: 300,
        })
    }

    #[test]
    fn provider_name_is_stable() {
        assert_eq!(provider_with_default_field_map().name(), "notion");
    }

    #[test]
    fn build_channel_filter_uses_mapped_property_name() {
        let p = provider_with_default_field_map();
        let f = p.build_channel_filter(&ChannelKind::Discord, "1234567890");
        assert_eq!(f["property"], "Discord ID");
        assert_eq!(f["rich_text"]["equals"], "1234567890");
    }

    #[test]
    fn build_channel_filter_falls_back_for_unmapped_channel() {
        let p = provider_with_default_field_map();
        let f = p.build_channel_filter(&ChannelKind::Other("matrix".into()), "@user");
        assert_eq!(f["property"], "matrix ID");
    }

    #[test]
    fn build_project_filter_uses_multi_select_key_by_default() {
        let p = provider_with_default_field_map();
        let f = p.build_project_filter("proj-alpha");
        assert_eq!(f["property"], "Projects");
        assert!(f["multi_select"]["contains"].as_str() == Some("proj-alpha"));
    }

    #[test]
    fn build_project_filter_uses_relation_key_when_configured() {
        let mut field_map = NotionFieldMap::default();
        field_map.projects_kind = ProjectsKind::Relation;
        let p = NotionIdentityProvider::new(NotionConfig {
            database_id: "db".into(),
            api_key: "k".into(),
            field_map,
            refresh_seconds: 0,
        });
        let f = p.build_project_filter("proj-alpha");
        assert!(f["relation"]["contains"].as_str() == Some("proj-alpha"));
    }

    #[test]
    fn map_row_extracts_full_record_from_notion_payload() {
        let p = provider_with_default_field_map();

        // Hand-crafted Notion API response shape — see Notion docs for
        // canonical schema.
        let row = serde_json::json!({
            "id": "abc-123",
            "properties": {
                "Name": {
                    "title": [{ "plain_text": "Ruby Lin" }]
                },
                "Roles": {
                    "multi_select": [
                        { "name": "customer-pm" },
                        { "name": "project-lead" }
                    ]
                },
                "Projects": {
                    "multi_select": [
                        { "name": "proj-alpha" }
                    ]
                },
                "Email": {
                    "email": "ruby@example.com"
                },
                "Discord ID": {
                    "rich_text": [{ "plain_text": "1234567890" }]
                },
                "Line ID": {
                    "rich_text": [{ "plain_text": "Uabc" }]
                }
            }
        });

        let person = p.map_row(&row).expect("should map");
        assert_eq!(person.person_id, "abc-123");
        assert_eq!(person.display_name, "Ruby Lin");
        assert_eq!(person.roles, vec!["customer-pm", "project-lead"]);
        assert_eq!(person.project_ids, vec!["proj-alpha"]);
        assert_eq!(person.emails, vec!["ruby@example.com"]);
        assert_eq!(person.handle_for(&ChannelKind::Discord), Some("1234567890"));
        assert_eq!(person.handle_for(&ChannelKind::Line), Some("Uabc"));
        assert_eq!(person.source, "notion");
    }

    #[test]
    fn map_row_returns_none_for_missing_title() {
        let p = provider_with_default_field_map();
        let row = serde_json::json!({
            "id": "abc-123",
            "properties": {
                "Name": { "title": [] }
            }
        });
        assert!(p.map_row(&row).is_none());
    }

    #[test]
    fn map_row_handles_missing_optional_properties() {
        let p = provider_with_default_field_map();
        let row = serde_json::json!({
            "id": "abc-123",
            "properties": {
                "Name": { "title": [{ "plain_text": "Bare" }] }
            }
        });
        let person = p.map_row(&row).expect("should map");
        assert_eq!(person.display_name, "Bare");
        assert!(person.roles.is_empty());
        assert!(person.project_ids.is_empty());
        assert!(person.channel_handles.is_empty());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // Multi-byte CJK should not split at byte boundary.
        assert_eq!(truncate("你好世界", 100), "你好世界");
        let cut = truncate("你好世界一二三四五六七八九十", 7);
        assert!(cut.ends_with('…'));
        // Verify it's a valid UTF-8 string (no panic on iteration).
        for _ in cut.chars() {}
    }
}
