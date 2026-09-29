//! One place that turns `config.toml [identity]` into a live
//! [`duduclaw_identity::IdentityProvider`] (G5, 2026-09 feature audit).
//!
//! ## What this closes
//!
//! RFC-21 §1 shipped three providers — wiki cache, Notion, and the chained
//! cache→upstream combination — and a dashboard page to configure which one is
//! in force. But only the **dashboard's own** `identity.resolve` RPC ever read
//! that setting. The two places that actually matter at runtime —
//! `channel_reply::build_sender_block` (the `<sender>` block injected into
//! every channel turn) and the `identity_resolve` MCP tool — both hard-coded
//! [`WikiCacheIdentityProvider`], each with a comment promising a later
//! migration step. So an operator could configure Notion, watch the dashboard
//! resolve people through it, and still have every agent see only the wiki
//! cache.
//!
//! [`build_identity_provider`] is that migration step: one free function over
//! `home_dir`, callable from a `MethodHandler`, from the channel reply path,
//! and from the MCP server in `duduclaw-cli`.
//!
//! ## Fail-safe selection
//!
//! `[identity] provider` is one of `wiki_cache` (default) / `notion` /
//! `chained`. Anything else — including a `notion` selection whose
//! `database_id` or API key is missing — degrades to the wiki cache rather
//! than erroring: an identity lookup that cannot reach its upstream should
//! make the sender look like a stranger, never break the turn.
//!
//! The returned label (`"wiki-cache"` / `"notion"` / `"chained"`) is what
//! actually took effect, not what was requested, so a degraded selection is
//! visible in logs and in the dashboard response.

use std::path::Path;
use std::sync::Arc;

use duduclaw_identity::IdentityProvider;
use duduclaw_identity::providers::{
    ChainedProvider, NotionConfig, NotionFieldMap, NotionIdentityProvider, ProjectsKind,
    WikiCacheIdentityProvider,
};

/// Default when `[identity] provider` is absent or unrecognised.
pub const DEFAULT_PROVIDER: &str = "wiki_cache";

/// Build the configured identity provider for `home_dir`.
///
/// Returns `(provider, effective_label)`. See the module docs for the
/// fail-safe rules — this never returns an error, because "cannot reach the
/// identity store" must degrade to "unknown sender", not to a failed turn.
pub async fn build_identity_provider(home_dir: &Path) -> (Arc<dyn IdentityProvider>, String) {
    let table = read_config_table(home_dir).await;
    build_from_table(&table, home_dir)
}

/// The pure-ish half of [`build_identity_provider`]: everything except the
/// `config.toml` read, so the selection rules are testable from a table
/// literal. (`home_dir` is still needed — the wiki cache reads from it and
/// the Notion key is decrypted against its keyfile.)
pub fn build_from_table(
    table: &toml::Table,
    home_dir: &Path,
) -> (Arc<dyn IdentityProvider>, String) {
    let wiki: Arc<dyn IdentityProvider> =
        Arc::new(WikiCacheIdentityProvider::for_home(home_dir.to_path_buf()));

    let idt = table.get("identity").and_then(|v| v.as_table());
    let selector = idt
        .and_then(|t| t.get("provider"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROVIDER);

    if selector != "notion" && selector != "chained" {
        return (wiki, "wiki-cache".to_string());
    }

    // Notion selected — try to build it. Any missing piece degrades to wiki.
    let Some(notion_tbl) = idt.and_then(|t| t.get("notion")).and_then(|v| v.as_table()) else {
        return (wiki, "wiki-cache".to_string());
    };
    let database_id = notion_tbl
        .get("database_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let api_key = notion_tbl
        .get("api_key_enc")
        .and_then(|v| v.as_str())
        .and_then(|enc| crate::config_crypto::decrypt_value(enc, home_dir))
        .or_else(|| {
            notion_tbl
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    if database_id.is_empty() || api_key.is_empty() {
        return (wiki, "wiki-cache".to_string());
    }

    let field_map = field_map_from_table(notion_tbl);
    let refresh_seconds = notion_tbl
        .get("refresh_seconds")
        .and_then(|v| v.as_integer())
        .unwrap_or(900)
        .max(0) as u64;

    let notion: Arc<dyn IdentityProvider> = Arc::new(NotionIdentityProvider::new(NotionConfig {
        database_id,
        api_key,
        field_map,
        refresh_seconds,
    }));

    if selector == "chained" {
        (
            Arc::new(ChainedProvider::new(wiki, notion)),
            "chained".to_string(),
        )
    } else {
        (notion, "notion".to_string())
    }
}

/// Defaults plus operator overrides for `[identity.notion.field_map]`.
fn field_map_from_table(notion_tbl: &toml::Table) -> NotionFieldMap {
    let mut field_map = NotionFieldMap::default();
    let Some(fm) = notion_tbl.get("field_map").and_then(|v| v.as_table()) else {
        return field_map;
    };
    if let Some(s) = fm.get("name").and_then(|v| v.as_str()) {
        field_map.name = s.to_string();
    }
    if let Some(s) = fm.get("roles").and_then(|v| v.as_str()) {
        field_map.roles = s.to_string();
    }
    if let Some(s) = fm.get("projects").and_then(|v| v.as_str()) {
        field_map.projects = s.to_string();
    }
    if let Some(s) = fm.get("emails").and_then(|v| v.as_str()) {
        field_map.emails = s.to_string();
    }
    if let Some(s) = fm.get("projects_kind").and_then(|v| v.as_str()) {
        field_map.projects_kind = match s {
            "relation" => ProjectsKind::Relation,
            _ => ProjectsKind::MultiSelect,
        };
    }
    if let Some(cp) = fm.get("channel_props").and_then(|v| v.as_table()) {
        let mut props = std::collections::BTreeMap::new();
        for (k, v) in cp {
            if let Some(s) = v.as_str() {
                props.insert(k.clone(), s.to_string());
            }
        }
        if !props.is_empty() {
            field_map.channel_props = props;
        }
    }
    field_map
}

async fn read_config_table(home_dir: &Path) -> toml::Table {
    match tokio::fs::read_to_string(home_dir.join("config.toml")).await {
        Ok(content) => content.parse::<toml::Table>().unwrap_or_default(),
        Err(_) => toml::Table::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> toml::Table {
        src.parse().expect("test config must parse")
    }

    #[test]
    fn absent_section_is_the_wiki_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let (p, label) = build_from_table(&toml::Table::new(), tmp.path());
        assert_eq!(label, "wiki-cache");
        assert_eq!(p.name(), "wiki-cache");
    }

    #[test]
    fn unknown_selector_degrades_to_wiki_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, label) = build_from_table(&table("[identity]\nprovider = \"ldap\"\n"), tmp.path());
        assert_eq!(label, "wiki-cache");
    }

    #[test]
    fn notion_without_credentials_degrades_to_wiki_cache() {
        // The important fail-safe: selecting Notion but leaving the database
        // id or key blank must not produce a provider that errors on every
        // turn — the sender simply stays unknown.
        let tmp = tempfile::tempdir().unwrap();
        let (_, label) = build_from_table(&table("[identity]\nprovider = \"notion\"\n"), tmp.path());
        assert_eq!(label, "wiki-cache");

        let (_, label) = build_from_table(
            &table("[identity]\nprovider = \"notion\"\n\n[identity.notion]\ndatabase_id = \"abc\"\n"),
            tmp.path(),
        );
        assert_eq!(label, "wiki-cache", "no api key ⇒ degrade");

        let (_, label) = build_from_table(
            &table("[identity]\nprovider = \"notion\"\n\n[identity.notion]\napi_key = \"secret_x\"\n"),
            tmp.path(),
        );
        assert_eq!(label, "wiki-cache", "no database id ⇒ degrade");
    }

    #[test]
    fn fully_configured_notion_and_chained_are_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = "[identity]\nprovider = \"notion\"\n\n[identity.notion]\ndatabase_id = \"db1\"\napi_key = \"secret_x\"\n";
        let (p, label) = build_from_table(&table(cfg), tmp.path());
        assert_eq!(label, "notion");
        assert_eq!(p.name(), "notion");

        let chained_cfg = cfg.replace("provider = \"notion\"", "provider = \"chained\"");
        let (p, label) = build_from_table(&table(&chained_cfg), tmp.path());
        assert_eq!(label, "chained");
        assert_eq!(p.name(), "chained");
    }

    #[test]
    fn field_map_overrides_are_applied() {
        let fm = field_map_from_table(
            &table(
                r#"
database_id = "db1"
api_key = "secret_x"
[field_map]
name = "姓名"
projects_kind = "relation"
[field_map.channel_props]
discord = "Discord ID"
"#,
            ),
        );
        assert_eq!(fm.name, "姓名");
        assert!(matches!(fm.projects_kind, ProjectsKind::Relation));
        assert_eq!(
            fm.channel_props.get("discord").map(String::as_str),
            Some("Discord ID")
        );
    }

    #[tokio::test]
    async fn reads_the_selection_from_config_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            "[identity]\nprovider = \"chained\"\n\n[identity.notion]\ndatabase_id = \"db1\"\napi_key = \"secret_x\"\n",
        )
        .unwrap();
        let (_, label) = build_identity_provider(tmp.path()).await;
        assert_eq!(label, "chained");
    }
}
