//! Gateway-owned, write-only Google STT credentials; never delegated to plugins.
use super::*;
use zeroize::{Zeroize, Zeroizing};

pub(super) enum Request {
    Set(Zeroizing<String>),
    Clear,
    Invalid(&'static str),
}

fn erase(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(erase),
        Value::Object(values) => values.values_mut().for_each(erase),
        _ => {},
    }
}

impl Request {
    fn action(&self) -> &'static str {
        match self { Self::Set(_) => "set", Self::Clear => "clear", Self::Invalid(action) => action }
    }

    pub(super) fn audit(&self, home: &Path, actor: &str, status: &str) {
        audit(home, self.action(), actor, status);
    }

    pub(super) fn take(method: &str, mut params: Value) -> Self {
        let action = match method {
            "transcription_credentials.set" => "set",
            "transcription_credentials.clear" => "clear",
            _ => "unknown",
        };
        let request = match method {
            "transcription_credentials.set" if params.as_object().is_some_and(|o| o.len() == 1) => {
                match params.get_mut("google_stt_key").map(Value::take) {
                    Some(Value::String(raw)) => Self::Set(Zeroizing::new(raw)),
                    Some(mut other) => { erase(&mut other); Self::Invalid(action) },
                    None => Self::Invalid(action),
                }
            },
            "transcription_credentials.clear" if params.as_object().is_some_and(|o| o.is_empty()) => Self::Clear,
            _ => Self::Invalid(action),
        };
        erase(&mut params);
        request
    }
}

/// Reject old, nested and dotted credential spellings before any params clone.
pub(super) fn reject_generic_credentials(params: &mut Value) -> bool {
    fn contains(value: &Value) -> bool {
        match value {
            Value::Object(fields) => fields.iter().any(|(key, value)| {
                key == "transcription_google_stt_key"
                    || key.split('.').any(|part| matches!(part,
                        "transcription_credentials" | "google_stt_key" | "google_stt_key_enc"))
                    || contains(value)
            }),
            Value::Array(values) => values.iter().any(contains),
            _ => false,
        }
    }
    if !contains(params) { return false; }
    erase(params);
    true
}

fn audit(home: &Path, action: &str, actor: &str, status: &str) {
    crate::security_autopilot::audit_and_emit(home, &duduclaw_security::audit::AuditEvent::new(
        &format!("transcription_credentials.{action}"), "transcription-credentials",
        duduclaw_security::audit::Severity::Info,
        json!({"actor_user_id": actor, "action": action, "status": status}),
    ));
}

impl MethodHandler {
    pub(super) async fn handle_transcription_credentials(&self, request: Request, ctx: &UserContext) -> WsFrame {
        // Match website credentials: direct callers also re-read the live role.
        if !crate::computer_use_sessions::live_ops::live_identity(&self.home_dir, ctx)
            .is_some_and(|live| live.is_admin()) {
            request.audit(&self.home_dir, &ctx.user_id, "refused");
            return WsFrame::error_response("", "只有管理員可以變更會議轉錄憑證。");
        }
        let action = request.action();
        let home = self.home_dir.clone();
        let actor = ctx.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<(), String> {
                let encrypted = match request {
                    Request::Set(raw) => {
                        if raw.is_empty() || duduclaw_inference::transcription::GoogleSttEngine::validate_service_account_json(&raw).is_err() {
                            return Err("服務帳戶憑證格式錯誤。".into());
                        }
                        Some(crate::config_crypto::encrypt_value(&raw, &home)
                            .ok_or("無法加密會議轉錄憑證。")?)
                    },
                    Request::Clear => None,
                    Request::Invalid(_) => return Err("會議轉錄憑證操作格式錯誤。".into()),
                };
                let path = home.join("config.toml");
                let original = config_commit::read_text_or_empty(&path)
                    .map_err(|_| "無法讀取會議轉錄設定。")?;
                let mut table: toml::Table = original.parse()
                    .map_err(|_| "會議轉錄設定格式錯誤。")?;
                let section = config_commit::table_at_mut(&mut table, &["transcription_credentials"])
                    .map_err(|_| "會議轉錄憑證設定格式錯誤。")?;
                section.remove("google_stt_key");
                section.remove("google_stt_key_enc");
                if let Some(encrypted) = encrypted {
                    section.insert("google_stt_key_enc".into(), toml::Value::String(encrypted));
                }
                let rendered = config_commit::render_preserving(&original, &table)
                    .map_err(|_| "無法儲存會議轉錄設定。")?;
                config_commit::commit_text_locked_with_guard(&path, &config_commit::content_hash(&original), &rendered, || {
                    super::system_update_config::require_live_admin_at_commit(&home, &actor)
                }).map_err(|_| "無法儲存會議轉錄憑證；請重新載入設定後再試。".into())
            })();
            // Keep the audit with the write even if the RPC future is dropped.
            audit(&home, action, &actor.user_id, if result.is_ok() { "success" } else { "refused" });
            result
        }).await;
        if result.is_err() {
            audit(&self.home_dir, action, &ctx.user_id, "failed");
        }
        match result {
            Ok(Ok(())) => WsFrame::ok_response("", json!({"success": true})),
            Ok(Err(message)) => WsFrame::error_response("", &message),
            Err(_) => WsFrame::error_response("", "會議轉錄憑證操作中斷。"),
        }
    }
}
