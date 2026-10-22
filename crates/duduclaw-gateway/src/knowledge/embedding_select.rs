//! `knowledge.embedding.select` — write `[knowledge.embedding]`.
//!
//! The write goes through the dashboard's locked, hash-checked,
//! format-preserving config commit. Selecting a cloud provider requires
//! `acknowledge_disclosure = true` and stores `cloud_disclosure_ack =
//! {by, at, provider, host, model}`; any other provider removes it, so an
//! old acknowledgement can never be re-used by flipping the provider back
//! in the raw editor.

use std::path::Path;

use duduclaw_knowledge::embed::catalog;
use duduclaw_knowledge::embed::config::{EmbeddingConfig, ProviderKind};
use serde_json::{Value, json};

use crate::handlers::config_commit::{commit_table_locked, content_hash, read_text_or_empty};

fn text(p: &Value, key: &str, max: usize) -> Result<Option<String>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.chars().count() > max || s.chars().any(char::is_control) {
                return Err(format!("{key} 格式不正確。"));
            }
            Ok(Some(s.to_string()))
        }
        Some(_) => Err(format!("{key} 必須是文字。")),
    }
}

fn embedding_table(root: &mut toml::Table) -> &mut toml::Table {
    let k = root
        .entry("knowledge")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if !k.is_table() {
        *k = toml::Value::Table(Default::default());
    }
    let k = k.as_table_mut().expect("table");
    let e = k
        .entry("embedding")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if !e.is_table() {
        *e = toml::Value::Table(Default::default());
    }
    e.as_table_mut().expect("table")
}

/// Apply the request to a parsed `config.toml`. Pure, for tests.
pub(crate) fn apply(
    root: &mut toml::Table,
    p: &Value,
    actor: &str,
    now: &str,
) -> Result<Value, String> {
    let provider_raw = text(p, "provider", 16)?.ok_or("缺少參數 provider。")?;
    let provider = super::embedding_rpc::provider_ok(&provider_raw)
        .ok_or("provider 只能是 local、openai、gemini 或 none。")?;
    let local_model = text(p, "local_model", 64)?;
    if let Some(m) = &local_model
        && catalog::find(m).is_none()
    {
        return Err("沒有這個本機向量模型。".to_string());
    }
    let cloud_model = text(p, "cloud_model", 64)?;
    if let Some(m) = &cloud_model
        && !m.is_empty()
        && !duduclaw_knowledge::embed::config::valid_model_id(m)
    {
        return Err("雲端模型名稱只能包含英數字、點、連字號與底線。".to_string());
    }
    let cloud_account = text(p, "cloud_account", 128)?;
    let cloud_base_url = text(p, "cloud_base_url", 300)?;
    // `allow_private_endpoint` is only ever set in the raw config editor
    // (Admin); this RPC reads it and never writes it.
    let allow_private = root
        .get("knowledge")
        .and_then(|k| k.get("embedding"))
        .and_then(|e| e.get("allow_private_endpoint"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    if let Some(u) = &cloud_base_url {
        super::embedding_rpc::base_url_ok(u, allow_private)?;
    }
    let t = embedding_table(root);
    let previous = t
        .get("provider")
        .and_then(toml::Value::as_str)
        .map(str::to_string);
    let provider_changed = previous.as_deref().unwrap_or("local") != provider.as_str();
    t.insert(
        "provider".into(),
        toml::Value::String(provider.as_str().into()),
    );
    if let Some(v) = &local_model {
        t.insert("local_model".into(), toml::Value::String(v.clone()));
    }
    // Cloud settings belong to one provider: a different provider starts
    // from its own defaults unless the request names them, so an endpoint or
    // model chosen for one vendor never carries over to another (and the
    // acknowledged host is the one that is really used).
    for (key, v) in [
        ("cloud_model", &cloud_model),
        ("cloud_account", &cloud_account),
        ("cloud_base_url", &cloud_base_url),
    ] {
        match v {
            Some(v) => {
                t.insert(key.into(), toml::Value::String(v.clone()));
            }
            None if provider_changed => {
                t.insert(key.into(), toml::Value::String(String::new()));
            }
            None => {}
        }
    }
    // Resolve what is now in effect from the edited table itself.
    let mut wrapper = toml::Table::new();
    let mut k = toml::Table::new();
    k.insert("embedding".into(), toml::Value::Table(t.clone()));
    wrapper.insert("knowledge".into(), toml::Value::Table(k));
    let effective =
        EmbeddingConfig::from_toml_str(&toml::to_string(&wrapper).map_err(|e| e.to_string())?);
    match provider {
        ProviderKind::Cloud(_) => {
            if p.get("acknowledge_disclosure").and_then(Value::as_bool) != Some(true) {
                return Err(
                    "使用雲端向量服務前，必須先確認文件段落（去識別化後）會送到該供應者。"
                        .to_string(),
                );
            }
            let target = effective.cloud_target().ok_or("雲端設定不完整。")??;
            let mut ack = toml::Table::new();
            for (key, v) in [
                ("by", actor.to_string()),
                ("at", now.to_string()),
                ("provider", target.provider.as_str().to_string()),
                ("host", target.host.clone()),
                ("model", target.model.clone()),
            ] {
                ack.insert(key.into(), toml::Value::String(v));
            }
            t.insert("cloud_disclosure_ack".into(), toml::Value::Table(ack));
            Ok(
                json!({"provider": provider.as_str(), "vendor": target.provider.vendor(), "host": target.host, "model": target.model}),
            )
        }
        _ => {
            t.remove("cloud_disclosure_ack");
            Ok(json!({"provider": provider.as_str(), "local_model": effective.local_model}))
        }
    }
}

/// `knowledge.embedding.select`.
pub async fn select(home: &Path, p: &Value, actor: &str) -> Result<Value, String> {
    let path = home.join("config.toml");
    let current = read_text_or_empty(&path)?;
    let hash = content_hash(&current);
    let mut root: toml::Table = if current.trim().is_empty() {
        toml::Table::new()
    } else {
        current
            .parse()
            .map_err(|_| "config.toml 無法解析，請先在設定頁修正。".to_string())?
    };
    let summary = apply(&mut root, p, actor, &duduclaw_knowledge::now_rfc3339())?;
    commit_table_locked(&path, hash, &root).await?;
    super::embedding::drop_cloud();
    super::embedding::drop_local();
    super::embedding::cache(home).clear();
    super::audit_admin(
        home,
        "knowledge_embedding_provider_changed",
        actor,
        summary.clone(),
    );
    let queued = super::embedding::schedule_all(home).await;
    Ok(json!({ "saved": true, "selection": summary, "queued": queued }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(start: &str, p: Value) -> Result<(toml::Table, Value), String> {
        let mut root: toml::Table = start.parse().unwrap();
        let v = apply(&mut root, &p, "admin", "2026-10-22T00:00:00Z")?;
        Ok((root, v))
    }

    #[test]
    fn cloud_needs_explicit_acknowledgement() {
        let err = run("", json!({"provider": "openai"})).unwrap_err();
        assert!(err.contains("確認"), "{err}");
        let err = run(
            "",
            json!({"provider": "gemini", "acknowledge_disclosure": "yes"}),
        )
        .unwrap_err();
        assert!(err.contains("確認"));
    }

    #[test]
    fn acknowledgement_binds_provider_host_and_model() {
        let (root, v) = run(
            "",
            json!({"provider": "openai", "acknowledge_disclosure": true}),
        )
        .unwrap();
        assert_eq!(v["host"], "api.openai.com");
        let text = toml::to_string(&root).unwrap();
        let cfg = EmbeddingConfig::from_toml_str(&text);
        let ack = cfg.ack.clone().unwrap();
        assert_eq!(
            (ack.by.as_str(), ack.provider.as_str(), ack.model.as_str()),
            ("admin", "openai", "text-embedding-3-small")
        );
        assert!(cfg.ack_covers(&cfg.cloud_target().unwrap().unwrap()));
        // Switching to local drops the record.
        let (root2, _) = run(&text, json!({"provider": "local"})).unwrap();
        let cfg2 = EmbeddingConfig::from_toml_str(&toml::to_string(&root2).unwrap());
        assert!(cfg2.ack.is_none());
        assert_eq!(cfg2.provider, ProviderKind::Local);
    }

    #[test]
    fn switching_provider_resets_the_previous_endpoint_and_model() {
        let start = "[knowledge.embedding]\nprovider = \"openai\"\ncloud_base_url = \"https://proxy.corp.example\"\ncloud_model = \"text-embedding-3-large\"\n";
        let (root, v) = run(
            start,
            json!({"provider": "gemini", "acknowledge_disclosure": true}),
        )
        .unwrap();
        assert_eq!(v["host"], "generativelanguage.googleapis.com", "{v}");
        assert_eq!(v["model"], "gemini-embedding-001");
        assert_eq!(
            root["knowledge"]["embedding"]["cloud_base_url"].as_str(),
            Some("")
        );
        // Same provider again: the configured endpoint is kept.
        let (_, v) = run(
            start,
            json!({"provider": "openai", "acknowledge_disclosure": true}),
        )
        .unwrap();
        assert_eq!(v["host"], "proxy.corp.example");
    }

    #[test]
    fn private_endpoints_need_the_raw_editor_switch() {
        let req = json!({"provider": "openai", "acknowledge_disclosure": true, "cloud_base_url": "http://127.0.0.1:9999"});
        let err = run("", req.clone()).unwrap_err();
        assert!(err.contains("allow_private_endpoint"), "{err}");
        assert!(run("", json!({"provider": "openai", "acknowledge_disclosure": true, "cloud_base_url": "https://169.254.169.254"})).is_err());
        let (_, v) = run(
            "[knowledge.embedding]\nallow_private_endpoint = true\n",
            req,
        )
        .unwrap();
        assert_eq!(v["host"], "127.0.0.1");
        // The RPC never turns the switch on itself.
        let (root, _) = run(
            "",
            json!({"provider": "local", "allow_private_endpoint": true}),
        )
        .unwrap();
        assert!(
            root["knowledge"]["embedding"]
                .get("allow_private_endpoint")
                .is_none()
        );
    }

    #[test]
    fn inputs_are_validated() {
        assert!(run("", json!({"provider": "voyage"})).is_err());
        assert!(run("", json!({"provider": "local", "local_model": "jina-v3"})).is_err());
        assert!(
            run(
                "",
                json!({"provider": "openai", "acknowledge_disclosure": true, "cloud_model": "x\"y"})
            )
            .is_err()
        );
        assert!(run("", json!({"provider": "openai", "acknowledge_disclosure": true, "cloud_base_url": "http://evil.example.com"})).is_err());
        let (root, v) = run(
            "[knowledge]\nenabled = true\n",
            json!({"provider": "gemini", "acknowledge_disclosure": true, "cloud_base_url": "https://proxy.example.com", "cloud_account": "acct-1"}),
        )
        .unwrap();
        assert_eq!(v["host"], "proxy.example.com");
        assert_eq!(
            root["knowledge"]["enabled"].as_bool(),
            Some(true),
            "other keys survive"
        );
        assert_eq!(
            root["knowledge"]["embedding"]["cloud_account"].as_str(),
            Some("acct-1")
        );
    }
}
