//! Shared fixtures for the gateway knowledge tests: employees on disk and
//! indexed collections with grants.

use std::path::Path;

use duduclaw_knowledge::acl::{PrincipalKind, Role, get_acl, set_acl};
use duduclaw_knowledge::extract::{Format, RawSegment, SegmentKind};
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::schema;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind, add_source};
use duduclaw_knowledge::store::{NewCollection, create_collection};
use serde_json::json;

/// Write `<home>/agents/<id>/agent.toml`.
pub(crate) fn agent(home: &Path, id: &str, toml: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

/// A collection owned by `owner` with `grants`, holding one indexed text
/// source titled `<slug>.txt`. Returns `(collection id, source id, chunk id)`.
pub(crate) fn collection(
    home: &Path,
    slug: &str,
    sensitivity: &str,
    grants: &[(PrincipalKind, &str, Role)],
    text: &str,
) -> (String, String, i64) {
    let mut conn = schema::open_home(home).unwrap();
    let cfg = KnowledgeConfig::default();
    let c = create_collection(
        &mut conn,
        &NewCollection {
            name: slug.into(),
            slug: Some(slug.into()),
            sensitivity: Some(sensitivity.into()),
            ..Default::default()
        },
        "owner",
        &cfg,
    )
    .unwrap();
    let (_, rev) = get_acl(&conn, &c.id).unwrap();
    let mut entries = vec![(PrincipalKind::User, "owner".to_string(), Role::Owner)];
    entries.extend(grants.iter().map(|(k, i, r)| (*k, i.to_string(), *r)));
    set_acl(&mut conn, &c.id, &entries, rev, "owner").unwrap();
    let new = NewSource {
        collection_id: c.id.clone(),
        kind: SourceKind::Paste,
        title: format!("{slug}.txt"),
        original_name: None,
        url: None,
        format: Format::Txt,
        blob_sha256: duduclaw_knowledge::sha256_hex(slug.as_bytes()),
        bytes: 1,
        origin: json!({}),
        created_by: "owner".into(),
    };
    let Added::New(s) = add_source(&mut conn, &new, &cfg).unwrap() else {
        panic!("duplicate source")
    };
    let prepared = duduclaw_knowledge::chunk::prepare(
        Format::Txt,
        vec![RawSegment::new(SegmentKind::Text, text.into())],
    );
    duduclaw_knowledge::index::write_index(&mut conn, &s.id, &prepared, &[], "t", &|_| false, &cfg)
        .unwrap();
    let chunk: i64 = conn
        .query_row("SELECT id FROM chunks WHERE source_id = ?1", [&s.id], |r| {
            r.get(0)
        })
        .unwrap();
    (c.id, s.id, chunk)
}
