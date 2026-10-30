//! Remove credentials before any preview, report or resolved payload exists.
use duduclaw_core::error::Result;

use super::ir::{AgentDraft, Doc, ImportBundle};
use crate::expert::cfg_err;

fn scan_error() -> duduclaw_core::error::DuDuClawError {
    cfg_err("憑證掃描失敗，未匯入".into())
}

pub(crate) fn mask_doc(doc: &mut Doc) -> Result<()> {
    mask_doc_with(doc, duduclaw_security::credential_detect::mask_document_with_count)
}

fn mask_doc_with(
    doc: &mut Doc,
    scan: impl FnOnce(&str) -> std::result::Result<(String, usize), duduclaw_security::credential_detect::DetectError>,
) -> Result<()> {
    if doc.text.len() as u64 > super::md::MAX_TEXT_BYTES { return Err(scan_error()); }
    let (masked, count) = scan(&doc.text).map_err(|_| scan_error())?;
    doc.text = masked;
    doc.masked_values += count;
    Ok(())
}

/// Settings retain their meaning or are refused, never silently rewritten.
pub(crate) fn reject_settings(settings: &toml::Table) -> Result<()> {
    fn walk(value: &toml::Value, path: &str, field: &str) -> Result<()> {
        match value {
            toml::Value::Table(table) => for (key, value) in table {
                let masked = duduclaw_security::credential_detect::mask_document(key).map_err(|_| scan_error())?;
                if masked != *key { return Err(cfg_err("設定欄位名稱含憑證，未匯入".into())); }
                walk(value, &format!("{path}.{key}"), key)?;
            },
            toml::Value::Array(values) => for (index, value) in values.iter().enumerate() {
                walk(value, &format!("{path}[{index}]"), field)?;
            },
            toml::Value::String(text) => {
                let key = field;
                let probe = format!("{key}: {text}");
                let masked = duduclaw_security::credential_detect::mask_document(&probe).map_err(|_| scan_error())?;
                let normalized: String = key.chars().filter(|c| c.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase).collect();
                let secret_field = matches!(normalized.as_str(), "password" | "passwd" | "pwd" | "token"
                    | "apikey" | "secret" | "clientsecret" | "accesstoken" | "refreshtoken"
                    | "authorization" | "connectionstring" | "privatekey");
                if masked != probe || (secret_field && !text.is_empty()) {
                    return Err(cfg_err(format!("設定欄位 {path} 含憑證，未匯入")));
                }
            },
            _ => {}
        }
        Ok(())
    }
    walk(&toml::Value::Table(settings.clone()), "duduclaw", "duduclaw")
}

pub(crate) fn sanitize_draft(draft: &mut AgentDraft) -> Result<()> {
    reject_settings(&draft.settings)?;
    // These settings/labels are not free text: changing them would change meaning.
    // Remove document bodies from this check; they are masked below instead.
    let mut metadata = serde_json::to_value(&*draft).map_err(|_| scan_error())?;
    if let Some(fields) = metadata.as_object_mut() {
        for field in ["soul", "identity", "user_profile", "instructions", "contract", "tools_notes",
            "heartbeat", "memory", "knowledge", "settings", "mcp_servers"] { fields.remove(field); }
    }
    reject_metadata(&metadata)?;
    for doc in [&mut draft.soul, &mut draft.identity, &mut draft.user_profile,
        &mut draft.instructions, &mut draft.contract, &mut draft.tools_notes, &mut draft.heartbeat]
        .into_iter().flatten().chain(draft.memory.iter_mut()).chain(draft.knowledge.iter_mut()) {
        reject_metadata(&serde_json::Value::String(doc.source.clone()))?;
        mask_doc(doc)?;
    }
    // Foreign MCP definitions are never installed; retain names only, not headers or env values.
    for (name, value) in &mut draft.mcp_servers {
        reject_metadata(&serde_json::Value::String(name.clone()))?;
        *value = serde_json::Value::Null;
    }
    Ok(())
}

pub(crate) fn mask_bytes(bytes: Vec<u8>) -> Result<Vec<u8>> {
    match std::str::from_utf8(&bytes) {
        Ok(text) => {
            if text.len() as u64 > super::md::MAX_TEXT_BYTES { return Err(scan_error()); }
            Ok(duduclaw_security::credential_detect::mask_document(text)
                .map_err(|_| scan_error())?.into_bytes())
        },
        Err(_) => Ok(bytes),
    }
}

fn reject_metadata(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::String(text) => {
            let masked = duduclaw_security::credential_detect::mask_document(text).map_err(|_| scan_error())?;
            if masked != *text { return Err(cfg_err("匯入中繼資料含憑證，未匯入".into())); }
        },
        serde_json::Value::Array(values) => for value in values { reject_metadata(value)?; },
        serde_json::Value::Object(values) => for (key, value) in values {
            reject_metadata(&serde_json::Value::String(key.clone()))?;
            reject_metadata(value)?;
        },
        _ => {}
    }
    Ok(())
}

pub(crate) fn sanitize_bundle(bundle: &mut ImportBundle) -> Result<()> {
    let mut metadata = serde_json::to_value(&*bundle).map_err(|_| scan_error())?;
    if let Some(fields) = metadata.as_object_mut() { fields.remove("agents"); }
    reject_metadata(&metadata)?;
    for draft in &mut bundle.agents { sanitize_draft(draft)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_failure_is_value_free_and_refuses() {
        let mut doc = Doc::new("USER.md", "password: hunter2");
        let error = mask_doc_with(&mut doc, |_| Err(duduclaw_security::credential_detect::DetectError)).unwrap_err();
        assert_eq!(error.to_string(), scan_error().to_string());
        assert_eq!(doc.text, "password: hunter2");
        assert!(!error.to_string().contains("hunter2"));
    }

    #[test]
    fn settings_refuse_credentials_with_field_names_only() {
        for text in ["password: hunter2".to_string(), ["sk-", "synthetic-credential-123456789"].concat()] {
            let mut model = toml::Table::new();
            model.insert("preferred".into(), toml::Value::String(text.clone()));
            let mut settings = toml::Table::new();
            settings.insert("model".into(), toml::Value::Table(model));
            let error = reject_settings(&settings).unwrap_err().to_string();
            assert!(error.contains("duduclaw.model.preferred"));
            assert!(!error.contains(&text));
        }
    }

    #[test]
    fn array_settings_keep_their_field_context() {
        let settings = toml::from_str("[environment]\npassword = ['hunter2']\nclient_secret = 'synthetic-value'").unwrap();
        let error = reject_settings(&settings).unwrap_err().to_string();
        assert!(error.contains("duduclaw.environment."));
        assert!(!error.contains("hunter2") && !error.contains("synthetic-value"));
        let settings = toml::from_str("[environment]\npassword = ['hunter2']").unwrap();
        let error = reject_settings(&settings).unwrap_err().to_string();
        assert!(error.contains("duduclaw.environment.password[0]"));
        assert!(!error.contains("hunter2"));
    }

    #[test]
    fn every_document_slot_is_masked_once_and_preview_keeps_counts() {
        let doc = || Some(Doc::new("profile.md", "password: hunter2"));
        let mut draft = AgentDraft { soul: doc(), identity: doc(), user_profile: doc(),
            instructions: doc(), contract: doc(), tools_notes: doc(), heartbeat: doc(),
            memory: vec![doc().unwrap()], knowledge: vec![doc().unwrap()], ..Default::default() };
        sanitize_draft(&mut draft).unwrap();
        let first = serde_json::to_string(&draft).unwrap();
        assert!(!first.contains("hunter2"));
        sanitize_draft(&mut draft).unwrap();
        assert_eq!(first, serde_json::to_string(&draft).unwrap());
        let preview = super::super::preview::sections(&[draft]).unwrap();
        for scan in preview[0]["input_guard"].as_array().unwrap().iter().take(5) {
            assert_eq!(scan["masked_values"], 1);
        }
    }
}
