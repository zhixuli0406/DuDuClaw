//! v1.68.0 — `tick.sources.list` / `tick.sources.upsert` /
//! `tick.sources.remove` (admin) and the tick-source hot reload.
//!
//! CRUD over `config.toml [[tick.sources]]`, validated with the same
//! `tick_config::validate_source` the boot loader uses, so a source the
//! dashboard accepts is a source the gateway will run. Header values are
//! credentials (plaintext or `secret://` references): they are accepted on
//! write and never returned — `list` reports `headers_count` only.
//!
//! Hot reload: the source tasks used to be spawned once at boot. `server.rs`
//! now hands the handler what it needs to respawn them
//! ([`MethodHandler::install_tick_runtime`]); after a write the tasks are
//! aborted and respawned from the new config. When the runtime was never
//! installed (autopilot engine not started) the response says
//! `restart_required: true`.

#[allow(unused_imports)]
use super::*;

use super::config_commit::{commit_table_locked, content_hash, is_secret_placeholder, read_text_or_empty};

/// Everything needed to (re)spawn the tick source tasks.
pub(crate) struct TickRuntime {
    tx: tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>,
    events_bus: Option<Arc<crate::events_store::EventBusStore>>,
    hub: Arc<crate::tick_source::TickHub>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}

/// Keys a `[[tick.sources]]` entry may carry, with the JSON shape accepted.
#[derive(Clone, Copy)]
enum FieldKind {
    Str,
    Bool,
    U64,
    StrArray,
    StrMap,
}

const SOURCE_FIELDS: &[(&str, FieldKind)] = &[
    ("id", FieldKind::Str),
    ("kind", FieldKind::Str),
    ("enabled", FieldKind::Bool),
    ("url", FieldKind::Str),
    ("command", FieldKind::StrArray),
    ("path", FieldKind::Str),
    ("subscribe", FieldKind::StrArray),
    ("interval_secs", FieldKind::U64),
    ("headers", FieldKind::StrMap),
    ("ping_interval_secs", FieldKind::U64),
    ("idle_timeout_secs", FieldKind::U64),
    ("json_fields", FieldKind::StrMap),
    ("emit_unchanged", FieldKind::Bool),
    ("max_events_per_minute", FieldKind::U64),
    ("persist_every_n", FieldKind::U64),
    ("baseline_max_age_secs", FieldKind::U64),
];

const MAX_TICK_SOURCES: usize = 64;

fn json_field_to_toml(key: &str, kind: FieldKind, v: &Value) -> Result<toml::Value, String> {
    let err = |what: &str| format!("tick source `{key}` must be {what}");
    match kind {
        FieldKind::Str => v.as_str().map(|s| toml::Value::String(s.trim().to_string())).ok_or_else(|| err("a string")),
        FieldKind::Bool => v.as_bool().map(toml::Value::Boolean).ok_or_else(|| err("a boolean")),
        FieldKind::U64 => v
            .as_u64()
            .and_then(|n| i64::try_from(n).ok())
            .map(toml::Value::Integer)
            .ok_or_else(|| err("a non-negative integer")),
        FieldKind::StrArray => v
            .as_array()
            .ok_or_else(|| err("an array of strings"))?
            .iter()
            .map(|x| x.as_str().map(|s| toml::Value::String(s.to_string())).ok_or_else(|| err("an array of strings")))
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        FieldKind::StrMap => {
            let obj = v.as_object().ok_or_else(|| err("an object of strings"))?;
            let mut t = toml::map::Map::new();
            for (k, x) in obj {
                let s = x.as_str().ok_or_else(|| err("an object of strings"))?;
                t.insert(k.clone(), toml::Value::String(s.to_string()));
            }
            Ok(toml::Value::Table(t))
        }
    }
}

/// Merge an upsert payload onto the stored entry (absent keys keep the
/// stored value, `null` removes the key). Header values equal to the secret
/// placeholder keep the stored value of that header; a `headers` object that
/// is sent replaces the stored map as a whole.
pub(crate) fn merge_source_entry(stored: Option<&toml::Table>, incoming: &serde_json::Map<String, Value>) -> Result<toml::Table, String> {
    if let Some(unknown) = incoming.keys().find(|k| !SOURCE_FIELDS.iter().any(|(f, _)| f == k)) {
        return Err(format!("unknown tick source field `{unknown}`"));
    }
    let mut entry = stored.cloned().unwrap_or_default();
    for (key, kind) in SOURCE_FIELDS {
        let Some(v) = incoming.get(*key) else { continue };
        if v.is_null() {
            entry.remove(*key);
            continue;
        }
        let mut tv = json_field_to_toml(key, *kind, v)?;
        if *key == "headers"
            && let toml::Value::Table(new_headers) = &mut tv
        {
            let old = stored.and_then(|s| s.get("headers")).and_then(|h| h.as_table());
            for (name, value) in new_headers.iter_mut() {
                if value.as_str().is_some_and(is_secret_placeholder) {
                    match old.and_then(|o| o.get(name)) {
                        Some(prev) => *value = prev.clone(),
                        None => return Err(format!("header `{name}` has no stored value to keep")),
                    }
                }
            }
        }
        entry.insert((*key).to_string(), tv);
    }
    // A stored header (credential) may only follow the source to the URL it
    // was entered for: when the URL changes, every kept header must be
    // re-entered (sent as a real value, not omitted or «set»).
    if let Some(stored) = stored {
        let url_changed = entry.get("url").is_some() && stored.get("url") != entry.get("url");
        let had_headers = stored.get("headers").and_then(|h| h.as_table()).is_some_and(|t| !t.is_empty());
        let headers_kept = match incoming.get("headers") {
            None => true,
            Some(Value::Null) => false,
            Some(Value::Object(o)) => o.values().any(|v| v.as_str().is_some_and(is_secret_placeholder)),
            Some(_) => false,
        };
        if url_changed && had_headers && headers_kept {
            return Err("the source URL changed — re-enter the header values (stored headers are not sent to a new URL)".into());
        }
    }
    Ok(entry)
}

/// Validate one raw entry exactly as the boot loader would (preset applied,
/// then `validate_source`).
pub(crate) fn validate_source_entry(entry: &toml::Table, tick: &toml::Table) -> Result<(), String> {
    let raw: crate::tick_config::TickSourceConfig = toml::Value::Table(entry.clone())
        .try_into()
        .map_err(|e| format!("invalid tick source: {e}"))?;
    let allow_command = tick.get("allow_command_sources").and_then(|v| v.as_bool()).unwrap_or(false);
    crate::tick_config::validate_source(raw, allow_command).map(|_| ())
}

/// JSON view of one stored entry: every field except header values.
fn source_view(entry: &toml::Table, tick: &toml::Table) -> Value {
    let mut obj = serde_json::Map::new();
    for (key, _) in SOURCE_FIELDS {
        if *key == "headers" {
            continue;
        }
        if let Some(v) = entry.get(*key).and_then(|v| serde_json::to_value(v).ok()) {
            // Credentials in the URL (userinfo, token-like query values)
            // are shown masked; header values are never shown at all.
            let v = match (*key, v) {
                ("url", Value::String(u)) => Value::String(super::config_raw_rpc::mask_url_for_display(&u)),
                (_, v) => v,
            };
            obj.insert((*key).to_string(), v);
        }
    }
    let headers_count = entry.get("headers").and_then(|h| h.as_table()).map_or(0, |t| t.len());
    obj.insert("headers_count".into(), json!(headers_count));
    obj.entry("enabled").or_insert(json!(true));
    let check = validate_source_entry(entry, tick);
    obj.insert("valid".into(), json!(check.is_ok()));
    obj.insert("error".into(), check.err().map_or(Value::Null, Value::String));
    Value::Object(obj)
}

fn tick_sources(table: &toml::Table) -> Vec<toml::Table> {
    table
        .get("tick")
        .and_then(|t| t.get("sources"))
        .and_then(|s| s.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_table().cloned()).collect())
        .unwrap_or_default()
}

fn entry_id(entry: &toml::Table) -> Option<&str> {
    entry.get("id").and_then(|v| v.as_str())
}

impl MethodHandler {
    /// Called once by `server.rs` where the autopilot bus exists: spawns the
    /// active sources and keeps the handles so a dashboard edit can respawn
    /// them. Returns how many source tasks are running.
    pub async fn install_tick_runtime(
        &self,
        tx: tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>,
        events_bus: Option<Arc<crate::events_store::EventBusStore>>,
        hub: Arc<crate::tick_source::TickHub>,
    ) -> usize {
        let cfg = crate::tick_config::TickConfig::from_home(&self.home_dir);
        let handles =
            crate::tick_source::spawn_tick_sources(&cfg, &self.home_dir, tx.clone(), hub.clone(), events_bus.clone());
        let n = handles.len();
        let mut slot = self.tick_runtime.lock().await;
        if let Some(old) = slot.take() {
            old.handles.iter().for_each(|h| h.abort());
        }
        *slot = Some(TickRuntime { tx, events_bus, hub, handles });
        n
    }

    /// Abort the running tick source tasks and spawn them again from the
    /// current `config.toml`. `None` when no runtime was installed (the
    /// caller then reports `restart_required`).
    pub(crate) async fn respawn_tick_sources(&self) -> Option<usize> {
        let mut slot = self.tick_runtime.lock().await;
        let rt = slot.as_mut()?;
        for h in rt.handles.drain(..) {
            h.abort();
        }
        let cfg = crate::tick_config::TickConfig::from_home(&self.home_dir);
        rt.handles = crate::tick_source::spawn_tick_sources(
            &cfg,
            &self.home_dir,
            rt.tx.clone(),
            rt.hub.clone(),
            rt.events_bus.clone(),
        );
        info!(sources = rt.handles.len(), "tick sources respawned after config change");
        Some(rt.handles.len())
    }

    /// `tick.sources.list` — every stored `[[tick.sources]]` entry (valid or
    /// not, with the validation error), plus the `[tick]` switches.
    pub(crate) async fn handle_tick_sources_list(&self) -> WsFrame {
        let path = self.home_dir.join("config.toml");
        let text = match read_text_or_empty(&path) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("config.toml is not valid TOML: {e}")),
        };
        let tick = table.get("tick").and_then(|t| t.as_table()).cloned().unwrap_or_default();
        let sources: Vec<Value> = tick_sources(&table).iter().map(|e| source_view(e, &tick)).collect();
        let runtime_installed = self.tick_runtime.lock().await.is_some();
        WsFrame::ok_response(
            "",
            json!({
                "enabled": tick.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                "allow_command_sources": tick.get("allow_command_sources").and_then(|v| v.as_bool()).unwrap_or(false),
                "preset": tick.get("preset").and_then(|v| v.as_str()),
                "dns_ttl_secs": tick.get("dns_ttl_secs").and_then(|v| v.as_integer()),
                "sources": sources,
                // false ⇒ edits are saved but only run after a restart.
                "hot_reload_available": runtime_installed,
            }),
        )
    }

    /// `tick.sources.upsert` — params: the source fields, either at the top
    /// level or under `source`. Keyed by `id`.
    pub(crate) async fn handle_tick_sources_upsert(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let incoming = match params.get("source").and_then(|v| v.as_object()).or_else(|| params.as_object()) {
            Some(o) => o.clone(),
            None => return WsFrame::error_response("", "params must be an object"),
        };
        let Some(id) = incoming.get("id").and_then(|v| v.as_str()).map(str::trim) else {
            return WsFrame::error_response("", "tick source `id` is required");
        };
        if !crate::tick_config::is_valid_source_id(id) {
            return WsFrame::error_response("", "tick source id must match ^[a-z0-9][a-z0-9-]{0,63}$");
        }
        let id = id.to_string();
        self.mutate_tick_sources(ctx, "tick.sources.upsert", move |sources, tick| {
            let pos = sources.iter().position(|e| entry_id(e) == Some(id.as_str()));
            let stored = pos.map(|i| sources[i].clone());
            let entry = merge_source_entry(stored.as_ref(), &incoming)?;
            if entry_id(&entry) != Some(id.as_str()) {
                return Err("tick source `id` cannot be changed by an upsert".into());
            }
            validate_source_entry(&entry, tick)?;
            let is_command = entry.get("kind").and_then(|v| v.as_str()) == Some("command");
            let command_changed = is_command && stored.as_ref().map(|s| s.get("command")) != Some(entry.get("command"));
            match pos {
                Some(i) => sources[i] = entry,
                None => {
                    if sources.len() >= MAX_TICK_SOURCES {
                        return Err(format!("at most {MAX_TICK_SOURCES} tick sources"));
                    }
                    sources.push(entry);
                }
            }
            Ok((format!("tick.sources[{id}] {}", if pos.is_some() { "updated" } else { "added" }), command_changed))
        })
        .await
    }

    /// `tick.sources.remove { id }`.
    pub(crate) async fn handle_tick_sources_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(id) = params.get("id").and_then(|v| v.as_str()).map(|s| s.trim().to_string()) else {
            return WsFrame::error_response("", "tick source `id` is required");
        };
        self.mutate_tick_sources(ctx, "tick.sources.remove", move |sources, _| {
            let before = sources.len();
            sources.retain(|e| entry_id(e) != Some(id.as_str()));
            if sources.len() == before {
                return Err(format!("no tick source with id `{id}`"));
            }
            Ok((format!("tick.sources[{id}] removed"), false))
        })
        .await
    }

    /// Read → mutate `[[tick.sources]]` → locked commit → audit → respawn.
    /// `mutate` returns the change line and whether a `command` source's argv
    /// was written (audited as a protected change).
    async fn mutate_tick_sources<F>(&self, ctx: &UserContext, rpc: &str, mutate: F) -> WsFrame
    where
        F: FnOnce(&mut Vec<toml::Table>, &toml::Table) -> Result<(String, bool), String>,
    {
        let path = self.home_dir.join("config.toml");
        let text = match read_text_or_empty(&path) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let mut table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("config.toml is not valid TOML, refusing to rewrite it: {e}")),
        };
        let tick = table.get("tick").and_then(|t| t.as_table()).cloned().unwrap_or_default();
        let mut sources = tick_sources(&table);
        let (change, command_written) = match mutate(&mut sources, &tick) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let tick_section = match super::config_commit::table_at_mut(&mut table, &["tick"]) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if sources.is_empty() {
            tick_section.remove("sources");
        } else {
            tick_section.insert(
                "sources".into(),
                toml::Value::Array(sources.into_iter().map(toml::Value::Table).collect()),
            );
        }
        if let Err(e) = commit_table_locked(&path, content_hash(&text), &table).await {
            return WsFrame::error_response("", &e);
        }
        let changes = vec![change];
        duduclaw_security::audit::log_config_changed(
            &self.home_dir,
            &ctx.email,
            &format!("{:?}", ctx.role).to_lowercase(),
            &changes,
        );
        if command_written {
            crate::security_autopilot::audit_and_emit(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "config_protected_key_changed",
                    &ctx.user_id,
                    duduclaw_security::audit::Severity::Warning,
                    json!({ "key": "tick.sources.command", "change": changes[0], "user_id": ctx.user_id, "source": rpc }),
                ),
            );
        }
        crate::security_autopilot::emit_config_changed();
        let tick_on = table
            .get("tick")
            .and_then(|t| t.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let respawned = self.respawn_tick_sources().await.is_some();
        info!(?changes, hot_reloaded = respawned && tick_on, "{rpc} completed");
        if !tick_on {
            // Saved, but resident sensing is off: nothing runs, so nothing
            // was hot-reloaded and nothing waits for a restart either.
            return WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "changes": changes,
                    "hot_reloaded": false,
                    "restart_required": false,
                    "note": "Saved. Sources are stored but not running until [tick] enabled is turned on.",
                }),
            );
        }
        WsFrame::ok_response(
            "",
            json!({ "success": true, "changes": changes, "hot_reloaded": respawned, "restart_required": !respawned }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn merge_keeps_unsent_fields_and_placeholder_headers() {
        let stored: toml::Table = toml::from_str(
            r#"id = "feed"
kind = "http_poll"
url = "https://example.com/x"
interval_secs = 30
headers = { "X-API-Key" = "secret-value" }
"#,
        )
        .unwrap();
        let merged = merge_source_entry(
            Some(&stored),
            &obj(json!({"id": "feed", "interval_secs": 60, "headers": {"X-API-Key": "«set»", "X-Extra": "secret://env/FOO"}})),
        )
        .unwrap();
        assert_eq!(merged.get("url").unwrap().as_str(), Some("https://example.com/x"));
        assert_eq!(merged.get("interval_secs").unwrap().as_integer(), Some(60));
        let h = merged.get("headers").unwrap().as_table().unwrap();
        assert_eq!(h.get("X-API-Key").unwrap().as_str(), Some("secret-value"));
        assert_eq!(h.get("X-Extra").unwrap().as_str(), Some("secret://env/FOO"));
    }

    #[test]
    fn merge_rejects_unknown_fields_and_bad_types() {
        assert!(merge_source_entry(None, &obj(json!({"id": "a", "bogus": 1}))).is_err());
        assert!(merge_source_entry(None, &obj(json!({"id": "a", "interval_secs": "10"}))).is_err());
        assert!(merge_source_entry(None, &obj(json!({"id": "a", "headers": {"X": "«set»"}}))).is_err());
    }

    #[test]
    fn validation_matches_the_boot_loader() {
        let tick = toml::Table::new();
        let ok: toml::Table = toml::from_str("id = \"a\"\nkind = \"websocket\"\nurl = \"wss://example.com/s\"\n").unwrap();
        assert!(validate_source_entry(&ok, &tick).is_ok());
        let cmd: toml::Table = toml::from_str("id = \"c\"\nkind = \"command\"\ncommand = [\"/bin/echo\"]\n").unwrap();
        assert!(validate_source_entry(&cmd, &tick).is_err(), "command needs allow_command_sources");
        let ssrf: toml::Table = toml::from_str("id = \"s\"\nkind = \"http_poll\"\nurl = \"http://127.0.0.1/x\"\n").unwrap();
        assert!(validate_source_entry(&ssrf, &tick).is_err());
    }

    #[test]
    fn view_hides_header_values() {
        let e: toml::Table = toml::from_str(
            "id = \"a\"\nkind = \"websocket\"\nurl = \"wss://example.com/s\"\nheaders = { Authorization = \"Bearer abc\" }\n",
        )
        .unwrap();
        let v = source_view(&e, &toml::Table::new());
        assert_eq!(v["headers_count"], 1);
        assert!(!v.to_string().contains("Bearer abc"));
        assert_eq!(v["valid"], true);
    }
}
