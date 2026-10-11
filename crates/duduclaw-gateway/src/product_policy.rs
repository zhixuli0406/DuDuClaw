//! Product installation policy shared by configuration and execution paths.
pub use duduclaw_core::product_profile::{load, ProductProfile};
use std::path::Path;

pub fn require_channel(home: &Path, id: &str) -> Result<(), String> {
    load(home)?.require_channel(id)
}
pub fn require_runtime(home: &Path, id: &str) -> Result<(), String> {
    load(home)?.require_runtime(id)
}
pub fn require_provider(home: &Path, id: &str) -> Result<(), String> {
    load(home)?.require_provider(id)
}

/// Check official typed selectors. A custom compatible URL is not classified by its name.
pub fn validate_table(policy: ProductProfile, table: &toml::Table) -> Result<(), String> {
    policy.check_available()?;
    if let Some(channels) = table.get("channels").and_then(|v| v.as_table()) {
        for key in channels.keys() {
            let value = &channels[key];
            if channel_configured(value) {
                policy.require_channel(key)?;
            }
            for id in policy.blocked_channels() {
                if key.starts_with(&format!("{id}_")) && channel_configured(value) {
                    policy.require_channel(id)?;
                }
            }
        }
    }
    if let Some(api) = table.get("api").and_then(|v| v.as_table()) {
        for id in api.keys() {
            policy.require_channel(id)?;
        }
        // Legacy global channel credential names remain in [api].
        for id in policy.blocked_channels() {
            if api.iter().any(|(key, value)| {
                (key == id || key.starts_with(&format!("{id}_"))) && channel_configured(value)
            }) {
                policy.require_channel(id)?;
            }
        }
    }
    if let Some(rt) = table.get("runtime").and_then(|v| v.as_table()) {
        for key in ["provider", "fallback", "utility_provider"] {
            if let Some(id) = rt.get(key).and_then(|v| v.as_str()) {
                policy.require_runtime(id)?;
            }
        }
    }
    if let Some(model) = table.get("model").and_then(|v| v.as_table()) {
        if let Some(id) = model.get("provider").and_then(|v| v.as_str()) {
            policy.require_provider(id)?;
        }
        for key in ["preferred", "fallback", "standard", "utility"] {
            if let Some(id) = model.get(key).and_then(|v| v.as_str()) {
                require_model(policy, id)?;
            }
        }
        if let Some(models) = model.get("fallbacks").and_then(|v| v.as_array()) {
            for model in models {
                if let Some(id) = model.as_str() {
                    require_model(policy, id)?;
                }
            }
        }
    }
    if let Some(voice) = table.get("voice").and_then(|v| v.as_table()) {
        for key in ["tts_provider", "stt_provider"] {
            if let Some(id) = voice.get(key).and_then(|v| v.as_str()) {
                policy.require_provider(id)?;
            }
        }
    }
    if let Some(accounts) = table.get("accounts").and_then(|v| v.as_array()) {
        for account in accounts {
            if let Some(id) = account.get("provider").and_then(|v| v.as_str()) {
                policy.require_provider(id)?;
            }
        }
    }
    Ok(())
}

fn channel_configured(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(v) => !v.is_empty(),
        toml::Value::Boolean(v) => *v,
        toml::Value::Table(t) => {
            if t.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
                return false;
            }
            t.get("enabled").and_then(toml::Value::as_bool) == Some(true)
                || t.iter()
                    .any(|(key, value)| key != "enabled" && channel_configured(value))
        }
        _ => false,
    }
}

pub fn require_model(policy: ProductProfile, model: &str) -> Result<(), String> {
    let rest = model.trim().strip_prefix("compat:").unwrap_or(model.trim());
    if let Some(info) = crate::cost_telemetry::model_registry().get(rest) {
        policy.require_provider(&info.provider)?;
    }
    let (provider, _) = duduclaw_llm::split_model_id(rest);
    if let Some(id) = provider {
        policy.require_provider(id)?;
    }
    Ok(())
}

/// Called before any dashboard write, including the raw editor and import paths.
/// The profile is installation-owned and cannot be replaced or removed by a save.
pub fn validate_write(home: &Path, file: &Path, before: &str, after: &str) -> Result<(), String> {
    let old: toml::Table = before
        .parse()
        .map_err(|_| "Existing configuration is invalid")?;
    let new: toml::Table = after
        .parse()
        .map_err(|_| "Submitted configuration is invalid")?;
    if file.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
        if old.get("product") != new.get("product") {
            return Err(
                "product.profile is installation-owned and cannot be changed by the dashboard"
                    .into(),
            );
        }
    } else if new.contains_key("product") && old.get("product") != new.get("product") {
        return Err("Product profile may only be configured by the installation".into());
    }
    validate_table(load(home)?, &new)
}

pub fn payload(home: &Path) -> Result<serde_json::Value, String> {
    let p = load(home)?;
    Ok(
        serde_json::json!({ "profile": p, "blocked_channels": p.blocked_channels(),
        "blocked_runtimes": p.blocked_runtimes(), "blocked_providers": p.blocked_providers() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_editor_cannot_remove_or_replace_installation_profile() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let before = "[product]\nprofile='restricted'\n";
        std::fs::write(&path, before).unwrap();
        for after in [
            "",
            "[product]\nprofile='standard'\n",
            "[product]\nprofile=''\n",
        ] {
            assert!(validate_write(temp.path(), &path, before, after).is_err());
        }
        assert!(validate_write(
            temp.path(),
            &path,
            before,
            &(before.to_owned() + "[gateway]\nport=9999\n")
        )
        .is_ok());
    }
    #[test]
    fn configuration_cannot_select_blocked_fallback_channel_or_account() {
        for text in [
            "[runtime]\nfallback='claude'",
            "[channels.slack]\nenabled=true",
            "[[accounts]]\nprovider='openai'",
            "[model]\nprovider='openai'",
            "[api]\nslack_corp_id='test'",
        ] {
            assert!(validate_table(ProductProfile::Restricted, &text.parse().unwrap()).is_err());
            assert!(validate_table(ProductProfile::Standard, &text.parse().unwrap()).is_ok());
        }
        assert!(validate_table(
            ProductProfile::Restricted,
            &"[model]\nprovider='anthropic'".parse().unwrap()
        )
        .is_ok());
    }
    #[test]
    fn malformed_installation_never_returns_unrestricted_policy() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("config.toml"), "[product]\nprofile='typo'").unwrap();
        assert!(payload(temp.path()).is_err());
        assert!(require_runtime(temp.path(), "claude").is_err());
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use crate::channel_sender::ChannelTarget;

    #[tokio::test]
    async fn sender_refuses_before_any_transport_attempt() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            "[product]\nprofile='restricted'",
        )
        .unwrap();
        for id in ["slack"] {
            let sender = crate::channel_sender::create_sender_for_home(
                temp.path(),
                &ChannelTarget {
                    channel_type: id.into(),
                    chat_id: "test".into(),
                    token: "test".into(),
                    extra_id: None,
                },
                reqwest::Client::new(),
            );
            assert_eq!(sender.channel_type(), "policy_blocked");
            assert!(sender
                .send_text("test")
                .await
                .unwrap_err()
                .to_string()
                .contains("Product profile"));
            assert!(sender.send_photo(&[], "test").await.is_err());
            assert!(sender.request_confirmation("test", None, 0).await.is_err());
        }
    }

    #[test]
    fn disabled_defaults_and_employee_names_are_not_provider_selectors() {
        let table: toml::Table = "[agent]\nname='openai'\n[channels.slack]\nenabled=false\n[channels.line]\nchannel_token='' \n[model]\nprovider='anthropic'\n".parse().unwrap();
        assert!(validate_table(ProductProfile::Restricted, &table).is_ok());
        assert!(require_model(ProductProfile::Restricted, "openai/openai-chat").is_err());
        assert!(require_model(ProductProfile::Restricted, "compat:openai/gpt-4o").is_err());
        assert!(require_model(ProductProfile::Restricted, "anthropic/claude-sonnet-4-6").is_ok());
        assert!(require_model(ProductProfile::Restricted, "custom/openai-themed-model").is_ok());
    }
}
