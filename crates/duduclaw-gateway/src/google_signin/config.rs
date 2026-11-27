//! Strict, uncached Google sign-in settings. Only the dedicated settings RPC
//! may create acknowledgement records; raw edits can only turn sign-in off.
use std::path::Path;

use crate::handlers::config_commit::{
    commit_rendered_locked, content_hash, read_text_or_empty, render_preserving,
};
use super::oidc::normalize_allowed_hd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval { Admin, Direct }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvatarTemplate { Blank, Company { key: String, revision: u64 }, Invalid }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoogleSigninSettings {
    pub enabled: bool,
    pub enabled_requested: bool,
    pub ack: Option<(String, String)>,
    pub allowed_hd: Vec<String>,
    pub approval: Approval,
    pub daily_new_accounts: u32,
    pub avatar_template: AvatarTemplate,
    pub approval_grants_mfa_enrollment: bool,
    pub revision: String,
}

#[derive(Clone, Debug)]
pub struct SettingsUpdate {
    pub enabled: bool,
    pub allowed_hd: Vec<String>,
    pub approval: Approval,
    pub daily_new_accounts: u32,
    pub avatar_template: AvatarTemplate,
    pub approval_grants_mfa_enrollment: bool,
}

// Sort every table recursively, including tables nested in arrays. The revision
// is lowercase hex SHA-256 of toml::to_string of this sorted section table;
// array order and unknown keys are significant, source formatting is not.
fn canonical(value: &toml::Value) -> toml::Value {
    match value {
        toml::Value::Table(table) => {
            let mut keys: Vec<_> = table.keys().collect();
            keys.sort();
            let mut sorted = toml::Table::new();
            for key in keys { sorted.insert(key.clone(), canonical(&table[key])); }
            toml::Value::Table(sorted)
        }
        toml::Value::Array(values) => toml::Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

fn domains(value: Option<&toml::Value>) -> Option<Vec<String>> {
    let mut result = Vec::new();
    for value in value?.as_array()? {
        let domain = normalize_allowed_hd(value.as_str()?)?;
        if !result.contains(&domain) { result.push(domain); }
    }
    Some(result)
}

fn from_root(root: &toml::Table) -> GoogleSigninSettings {
    let empty = toml::Table::new();
    let table = root.get("google_signin").and_then(toml::Value::as_table).unwrap_or(&empty);
    let encoded = toml::to_string(&canonical(&toml::Value::Table(table.clone())));
    let serializable = encoded.is_ok();
    let revision = content_hash(&encoded.unwrap_or_default());
    let enabled_requested = table.get("enabled").and_then(toml::Value::as_bool) == Some(true);
    let ack = table.get("enabled_ack").and_then(toml::Value::as_table).and_then(|ack| {
        let by = ack.get("by")?.as_str()?;
        let at = ack.get("at")?.as_str()?;
        if by.trim().is_empty() || at.trim().is_empty() { None }
        else { Some((by.to_owned(), at.to_owned())) }
    });
    let allowed_hd = domains(table.get("allowed_hd")).unwrap_or_default();
    let avatar_template = match table.get("avatar_template") {
        None => AvatarTemplate::Blank,
        Some(toml::Value::String(s)) if s.is_empty() => AvatarTemplate::Blank,
        Some(toml::Value::String(s)) => {
            match (s.strip_prefix("company:"), table.get("avatar_template_revision").and_then(toml::Value::as_integer)) {
                (Some(key), Some(revision)) if crate::company_templates::identifier(key) && revision > 0 =>
                    AvatarTemplate::Company { key: key.into(), revision: revision as u64 },
                _ => AvatarTemplate::Invalid,
            }
        }
        _ => AvatarTemplate::Invalid,
    };
    GoogleSigninSettings {
        enabled: serializable && enabled_requested && ack.is_some() && !allowed_hd.is_empty(),
        enabled_requested, ack, allowed_hd,
        approval: if table.get("approval").and_then(toml::Value::as_str) == Some("direct") { Approval::Direct } else { Approval::Admin },
        daily_new_accounts: match table.get("daily_new_accounts") {
            None => 20,
            Some(value) => value.as_integer().filter(|n| (1..=200).contains(n)).map(|n| n as u32).unwrap_or(0),
        },
        avatar_template,
        approval_grants_mfa_enrollment: table.get("approval_grants_mfa_enrollment").and_then(toml::Value::as_bool) == Some(true),
        revision,
    }
}

/// Re-read on every start/callback/complete. Uses the same direct file access as
/// runtime_config::load_runtime_settings_strict; no cached or lenient config.
pub fn read_strict(home: &Path) -> GoogleSigninSettings {
    let root = std::fs::read_to_string(home.join("config.toml")).ok()
        .and_then(|text| text.parse::<toml::Table>().ok()).unwrap_or_default();
    from_root(&root)
}

/// The only settings write allowed without step-up; it can only narrow.
/// Reads and edits the current settings inside the locked render callback.
/// A missing section or an already disabled section without an acknowledgement
/// commits unchanged text. Existing sections always lose their acknowledgement;
/// no actor or timestamp acknowledgement is created by this operation.
pub fn disable_settings(
    home: &Path, _actor_user_id: &str, _now_rfc3339: &str,
) -> Result<(GoogleSigninSettings, GoogleSigninSettings), String> {
    let path = home.join("config.toml");
    let current = read_text_or_empty(&path).map_err(|_| "無法讀取設定，請稍後再試。")?;
    let mut result = None;
    commit_rendered_locked(&path, &content_hash(&current), |text| {
        let mut root: toml::Table = text.parse().map_err(|_| "設定內容格式不正確，請先到設定頁修正。")?;
        let before = from_root(&root);
        if let Some(section) = root.get_mut("google_signin") {
            let table = section.as_table_mut().ok_or("設定內容格式不正確，請先到設定頁修正。")?;
            table.insert("enabled".into(), false.into());
            table.remove("enabled_ack");
        }
        let after = from_root(&root);
        let rendered = if before == after { text.to_owned() } else { render_preserving(text, &root)? };
        result = Some((before, after));
        Ok(rendered)
    }).map_err(|_| "無法儲存設定，設定可能已變更，請重新載入後再試。")?;
    result.ok_or_else(|| "無法儲存設定，請稍後再試。".into())
}

/// The caller (admin RPC) must enforce a live Admin session, step-up,
/// the risk acknowledgement and the audit row.
/// This synchronous adapter uses the same locked, hash-checked and
/// format-preserving commit engine as commit_table_locked, without a runtime.
pub fn write_settings(
    home: &Path, actor_user_id: &str, update: &SettingsUpdate,
    acknowledge_risk: bool, now_rfc3339: &str,
) -> Result<(GoogleSigninSettings, GoogleSigninSettings), String> {
    if !(1..=50).contains(&update.allowed_hd.len()) {
        return Err("請填寫一到五十個允許登入的公司網域。".into());
    }
    let mut allowed_hd = Vec::new();
    for domain in &update.allowed_hd {
        let domain = normalize_allowed_hd(domain).ok_or("公司網域格式不正確，請重新填寫。")?;
        if !allowed_hd.contains(&domain) { allowed_hd.push(domain); }
    }
    if !(1..=200).contains(&update.daily_new_accounts) {
        return Err("每天可建立的帳號數量必須介於一到兩百。".into());
    }
    let (template, template_revision) = match &update.avatar_template {
        AvatarTemplate::Blank => (String::new(), 0),
        AvatarTemplate::Company { key, revision } if crate::company_templates::identifier(key) && *revision > 0 && *revision <= i64::MAX as u64 =>
            (format!("company:{key}"), *revision as i64),
        _ => return Err("請選擇有效的公司分身範本，或使用空白分身。".into()),
    };
    let path = home.join("config.toml");
    let current = read_text_or_empty(&path).map_err(|_| "無法讀取設定，請稍後再試。")?;
    let mut root: toml::Table = current.parse().map_err(|_| "設定內容格式不正確，請先到設定頁修正。")?;
    let before = from_root(&root);
    let risk_changed = (update.enabled && !before.enabled)
        || (update.approval == Approval::Direct && before.approval != Approval::Direct)
        || (update.approval_grants_mfa_enrollment && !before.approval_grants_mfa_enrollment);
    if risk_changed && !acknowledge_risk {
        return Err("請先確認已了解開啟 Google 登入、直接建立帳號或預先授權兩步驟驗證註冊的風險。".into());
    }
    let settings_changed = allowed_hd != before.allowed_hd
        || update.approval != before.approval
        || update.daily_new_accounts != before.daily_new_accounts
        || update.avatar_template != before.avatar_template
        || update.approval_grants_mfa_enrollment != before.approval_grants_mfa_enrollment;
    let refresh_ack = update.enabled && (!before.enabled || (settings_changed && acknowledge_risk));
    if refresh_ack && (actor_user_id.trim().is_empty() || now_rfc3339.trim().is_empty()) {
        return Err("無法記錄風險確認，請重新登入後再試。".into());
    }
    let mut table = root.get("google_signin").and_then(toml::Value::as_table).cloned().unwrap_or_default();
    table.insert("enabled".into(), update.enabled.into());
    table.insert("allowed_hd".into(), toml::Value::Array(allowed_hd.into_iter().map(toml::Value::String).collect()));
    table.insert("approval".into(), (if update.approval == Approval::Direct { "direct" } else { "admin" }).into());
    table.insert("daily_new_accounts".into(), i64::from(update.daily_new_accounts).into());
    table.insert("avatar_template".into(), template.into());
    table.insert("avatar_template_revision".into(), template_revision.into());
    table.insert("approval_grants_mfa_enrollment".into(), update.approval_grants_mfa_enrollment.into());
    if !update.enabled { table.remove("enabled_ack"); }
    else if refresh_ack {
        let mut ack = toml::Table::new();
        ack.insert("by".into(), actor_user_id.into());
        ack.insert("at".into(), now_rfc3339.into());
        table.insert("enabled_ack".into(), toml::Value::Table(ack));
    }
    root.insert("google_signin".into(), toml::Value::Table(table));
    let after = from_root(&root);
    commit_rendered_locked(&path, &content_hash(&current), |text| render_preserving(text, &root))
        .map_err(|_| "無法儲存設定，設定可能已變更，請重新載入後再試。")?;
    Ok((before, after))
}

/// Only section deletion, enabled=false and acknowledgement removal are raw edits.
pub(crate) fn refuse_google_signin_widening(before: &toml::Table, after: &toml::Table) -> Result<(), String> {
    let old = before.get("google_signin");
    let new = after.get("google_signin");
    if new.is_none() || old == new { return Ok(()); }
    if let Some(new) = new.and_then(toml::Value::as_table) {
        let empty = toml::Table::new();
        let old_table = match old {
            Some(toml::Value::Table(table)) => Some(table),
            None if new.get("enabled") == Some(&toml::Value::Boolean(false)) => Some(&empty),
            _ => None,
        };
        let Some(old) = old_table else {
            return Err("請到 Google 設定頁調整登入設定；這裡只能關閉登入或移除風險確認紀錄。".into());
        };
        let mut narrowed = old.clone();
        if new.get("enabled") == Some(&toml::Value::Boolean(false)) {
            narrowed.insert("enabled".into(), false.into());
        }
        if !new.contains_key("enabled_ack") { narrowed.remove("enabled_ack"); }
        if &narrowed == new { return Ok(()); }
    }
    Err("Google sign-in cannot be expanded here; use Settings → Google. 請到 Google 設定頁調整登入設定；這裡只能關閉登入或移除風險確認紀錄。".into())
}
