//! `google.identity.test {agent_id}`: prove a delegate works without sending
//! anything (design §7.3). Every step mints fresh (the cache is bypassed and
//! never written), reads only GET endpoints, and reports one line per check.
//! Explicit mailbox diagnostics use Drive metadata and service APIs.
//! `delegate:owner` uses tokeninfo for ordinary scopes and one metadata-only
//! Drive probe when Drive write is enabled.

use std::path::Path;

use duduclaw_core::google_identity::{self as gi, Effective};
use serde::Serialize;
use serde_json::Value;

use super::requester_gate::People;
use super::resolve::{mint_delegate, precheck, refusal_for};
use crate::google_service_account::CacheMode;
use crate::google_workspace as gw;

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: String,
    pub label: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Per-service probe: scope group and a read-only list endpoint. Document
/// services without a list endpoint discover one sample through Drive.
const SERVICES: &[(&str, &str, &[&str], Option<&str>)] = &[
    (
        "gmail",
        "Gmail",
        &["https://www.googleapis.com/auth/gmail.readonly"],
        Some("https://gmail.googleapis.com/gmail/v1/users/me/profile"),
    ),
    (
        "calendar",
        "日曆",
        &["https://www.googleapis.com/auth/calendar.events"],
        Some("https://www.googleapis.com/calendar/v3/calendars/primary/events?maxResults=1"),
    ),
    (
        "drive",
        "雲端硬碟",
        &["https://www.googleapis.com/auth/drive.readonly"],
        Some("https://www.googleapis.com/drive/v3/files?pageSize=1"),
    ),
    (
        "sheets",
        "試算表",
        &["https://www.googleapis.com/auth/spreadsheets"],
        None,
    ),
    (
        "docs",
        "文件",
        &["https://www.googleapis.com/auth/documents"],
        None,
    ),
    (
        "slides",
        "簡報",
        &["https://www.googleapis.com/auth/presentations.readonly"],
        None,
    ),
    (
        "forms",
        "表單",
        &[
            "https://www.googleapis.com/auth/forms.body.readonly",
            "https://www.googleapis.com/auth/forms.responses.readonly",
        ],
        None,
    ),
    (
        "tasks",
        "工作清單",
        &["https://www.googleapis.com/auth/tasks"],
        Some("https://tasks.googleapis.com/tasks/v1/users/@me/lists?maxResults=1"),
    ),
];

fn step(id: &'static str, label: &'static str, ok: bool, detail: impl Into<String>) -> Step {
    Step {
        id: id.into(),
        label,
        ok,
        detail: detail.into(),
    }
}

/// Revalidate the configured identity around every diagnostic request. In
/// particular an owner change or revoked attestation must stop further reads.
struct DiagnosticIdentity<'a> {
    home: &'a Path,
    agent: &'a str,
    email: &'a str,
    expected: &'a gi::EffectiveIdentity,
    configured: &'a gi::EffectiveIdentity,
    people: &'a dyn People,
}

impl DiagnosticIdentity<'_> {
    fn verify(&self) -> Result<(), super::Refusal> {
        if gi::effective_for(self.home, Some(self.agent)) != *self.configured
            || super::delegate_owner::effective_for(self.home, Some(self.agent)) != *self.expected {
            return Err(super::Refusal::hard("identity_changed", "身分、負責人或信箱認證已變更，請管理員重新確認。"));
        }
        precheck(self.home, self.email, self.people)?;
        if crate::runtime_readiness::is_protected_linux() {
            protected_subject_gate(self.home, self.agent, self.people)
                .map_err(|message| super::Refusal::hard("attestation_required", message))?;
        }
        Ok(())
    }

    async fn mint(&self, scopes: &[&str]) -> Result<crate::google_service_account::MintedToken, super::Refusal> {
        self.verify()?;
        let result = mint_delegate(self.home, self.email, scopes, CacheMode::Bypass).await;
        self.verify()?;
        result
    }

    async fn get(&self, token: &str, url: &str) -> Result<Value, String> {
        self.verify().map_err(|r| r.message_zh)?;
        let result = gw::api_get(token, url).await.map_err(|e| e.to_string());
        self.verify().map_err(|r| r.message_zh)?;
        result
    }
}

/// Only the resource id crosses this boundary; no document content, title or
/// sample id is returned to the dashboard. No sample means unverified, never
/// that issuing a token proved the service's data API works.
async fn resource_probe(
    identity: &DiagnosticIdentity<'_>,
    id: &str,
    discovery_token: &str,
    service_token: &str,
) -> Result<String, String> {
    let (mime, endpoint, field) = match id {
        "sheets" => (
            "application/vnd.google-apps.spreadsheet",
            "https://sheets.googleapis.com/v4/spreadsheets",
            "spreadsheetId",
        ),
        "docs" => (
            "application/vnd.google-apps.document",
            "https://docs.googleapis.com/v1/documents",
            "documentId",
        ),
        "slides" => (
            "application/vnd.google-apps.presentation",
            "https://slides.googleapis.com/v1/presentations",
            "presentationId",
        ),
        "forms" => (
            "application/vnd.google-apps.form",
            "https://forms.googleapis.com/v1/forms",
            "formId",
        ),
        _ => return Err("沒有可用的唯讀驗證方法。".into()),
    };
    let mut lookup = url::Url::parse("https://www.googleapis.com/drive/v3/files")
        .map_err(|_| "無法準備唯讀驗證。")?;
    lookup
        .query_pairs_mut()
        .append_pair("q", &format!("trashed = false and mimeType = '{mime}'"))
        .append_pair("pageSize", "1")
        .append_pair("fields", "files(id)");
    let files = identity.get(discovery_token, lookup.as_str())
        .await
        .map_err(|_| "無法列出可用的驗證檔案；尚未確認服務資料存取。")?;
    let sample = files.get("files").and_then(Value::as_array).and_then(|rows| rows.first())
        .and_then(|file| file.get("id")).and_then(Value::as_str)
        .ok_or("授權已簽發，但沒有找到這類檔案，尚未確認服務資料存取。請先準備一份本人可讀的檔案再測試。")?;
    if sample.is_empty()
        || sample.len() > 256
        || !sample
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Google 回傳的驗證檔案識別碼無法辨識，尚未確認服務資料存取。".into());
    }
    let result = identity.get(
        service_token,
        &format!("{endpoint}/{sample}?fields={field}"),
    )
    .await
    .map_err(|_| "授權已簽發，但讀取這項服務的檔案資料失敗；請確認 API 已開通及檔案權限。")?;
    if result.get(field).and_then(Value::as_str) != Some(sample) {
        return Err("服務回應無法辨識，尚未確認資料存取。".into());
    }
    Ok("可讀取這項服務的檔案識別資料（未讀取內容）。".into())
}

/// SR4 round 3 (item 5 residual): the protected-mode gate of the identity
/// test — the same D-5 standing the bridge requires (an attested person of
/// the live employee incarnation, or a designated service mailbox). Blocking
/// reads (users.db, lifecycle).
fn protected_subject_gate(home: &Path, agent: &str, people: &dyn People) -> Result<(), String> {
    super::tenant_source::admit_for_identity_test(home, agent, people.user_db(),
        super::tenant_source::current_incarnation(home, agent))
}

#[cfg(test)]
tokio::task_local! {
    pub(super) static WRITE_TEST_API_BASE: url::Url;
}

/// The write token is consumed here and is never exposed by the diagnostic.
pub(in crate::google_identity) async fn drive_write_step(
    home: &Path, agent: &str, people: &dyn People,
    configured: &gi::EffectiveIdentity, effective: &gi::EffectiveIdentity,
) -> Step {
    use super::drive_write::transport::{WriteCall, WriteResult, WriteTransport};
    use crate::person_credentials::{write_store, Status};
    let report = |ok, detail| step("drive_write", "雲端硬碟寫入", ok, detail);
    const FAILED: &str = "無法確認雲端硬碟寫入連線，請管理員檢查後再試。";
    if !gw::drive_write_policy::load(home).any_enabled() {
        // Disabled optional diagnostics do not fail otherwise healthy identities.
        return report(true, "管理員尚未開放");
    }
    let home_path = matches!(effective.effective, Effective::Home)
        .then(|| gw::selected_path(home).ok()).flatten();
    let unchanged = || gi::effective_for(home, Some(agent)) == *configured
        && super::delegate_owner::effective_for(home, Some(agent)) == *effective
        && (!matches!(effective.effective, Effective::Home)
            || gw::selected_path(home).ok() == home_path)
        && gw::drive_write_policy::load(home).any_enabled();
    if !unchanged() { return report(false, FAILED); }
    if let Effective::Personal { selector } = &effective.effective {
        let Some(db) = people.user_db() else { return report(false, FAILED); };
        let Ok(user) = crate::google_personal::access::select_user(db, agent, selector) else {
            return report(false, FAILED);
        };
        if user.status != duduclaw_auth::UserStatus::Active { return report(false, "需要重新連結"); }
        let snapshot = write_store::snapshot(home, "user", &user.id);
        if !unchanged() { return report(false, FAILED); }
        return match snapshot {
            Ok(Some(snapshot)) => match snapshot.status {
                Status::Connected => report(true, "已連結"),
                _ => report(false, "需要重新連結"),
            },
            Ok(None) => report(false, "尚未連結"),
            Err(_) => report(false, FAILED),
        };
    }
    let identity = match &effective.effective {
        Effective::Delegate { email } => Some(DiagnosticIdentity {
            home, agent, email, expected: effective, configured, people,
        }),
        Effective::Home => None,
        _ => return report(false, FAILED),
    };
    let verify = || -> Result<(), super::Refusal> {
        if !unchanged() { return Err(super::Refusal::hard("identity_changed", FAILED)); }
        if let Some(identity) = &identity { identity.verify()?; }
        Ok(())
    };
    if verify().is_err() { return report(false, FAILED); }
    let minted = if let Some(identity) = &identity {
        super::resolve::mint_delegate_for_drive_write(home, identity.email, CacheMode::Bypass).await
            .map(|token| zeroize::Zeroizing::new(token.access_token))
    } else {
        match home_path.as_deref() {
            Some("oauth") => {
                use super::drive_write::credential::{self, IdentityRoute};
                credential::write_token(home, &IdentityRoute::HomeOAuth).await.map(|token| token.bearer)
            }
            Some("apps_script") => return report(false, "這種公司帳號連線方式不支援雲端硬碟寫入"),
            Some("service_account") => super::home_source::home_sa_token_for_drive_write(home).await
                .map(|token| zeroize::Zeroizing::new(token.access_token)),
            _ => return report(false, FAILED),
        }
    };
    if verify().is_err() { return report(false, FAILED); }
    let token = match minted {
        Ok(token) => token,
        Err(refusal) if matches!(refusal.code, "drive_write_admin_scope_missing" | "drive_write_needs_home_consent") => {
            return step("drive_write", "雲端硬碟寫入", false, refusal.message_zh);
        }
        Err(_) => return report(false, FAILED),
    };
    let Ok(mut transport) = WriteTransport::new(token) else {
        return report(false, FAILED);
    };
    #[cfg(test)]
    if let Ok(base) = WRITE_TEST_API_BASE.try_with(Clone::clone) {
        if transport.set_test_api_base(base).is_err() { return report(false, FAILED); }
    }
    if verify().is_err() { return report(false, FAILED); }
    let result = transport.send(&WriteCall::DriveProbe).await;
    if verify().is_err() { return report(false, FAILED); }
    match result {
        WriteResult::Ok((status, _)) if (200..300).contains(&status) => report(true, "可以使用"),
        _ => report(false, FAILED),
    }
}

/// Run the checks for `agent`. Stops at the first check later ones depend on.
pub async fn run(home: &Path, agent: &str, people: &dyn People) -> (Option<String>, Vec<Step>) {
    let configured = gi::effective_for(home, Some(agent));
    let effective = super::delegate_owner::effective_for(home, Some(agent));
    run_selected(home, agent, people, &configured, &effective).await
}

pub(crate) async fn run_selected(home: &Path, agent: &str, people: &dyn People,
    configured: &gi::EffectiveIdentity, eff: &gi::EffectiveIdentity) -> (Option<String>, Vec<Step>) {
    let mut out = vec![drive_write_step(home, agent, people, configured, eff).await];
    let email = match &eff.effective {
        Effective::Delegate { email } => email.clone(),
        Effective::Home => {
            out.push(step(
                "identity",
                "身分設定",
                true,
                "這位 AI 員工用公司共用帳號，請用整合頁的「測試」檢查共用帳號。",
            ));
            return (None, out);
        }
        other => {
            let r = refusal_for(other).map(|r| r.message_zh).unwrap_or_default();
            out.push(step("identity", "身分設定", false, r));
            return (None, out);
        }
    };
    out.push(step(
        "identity",
        "身分設定",
        true,
        format!("公司授權代表 {email}"),
    ));

    match precheck(home, &email, people) {
        Ok(()) => out.push(step("domain", "網域與帳號狀態", true, "在允許的網域內")),
        Err(r) => {
            out.push(step("domain", "網域與帳號狀態", false, r.message_zh));
            return (Some(email), out);
        }
    }

    // SR4 round 3 (item 5 residual): in protected mode the D-5 subject rule
    // applies before anything is minted. A delegate person no Admin attested
    // for this employee is never tested (minting would act as that person).
    if crate::runtime_readiness::is_protected_linux() {
        if let Err(message) = protected_subject_gate(home, agent, people) {
            out.push(step("attestation", "管理員確認", false, message));
            return (Some(email), out);
        }
        out.push(step("attestation", "管理員確認", true, "已符合受保護模式的代表規則"));
    }

    let identity = DiagnosticIdentity { home, agent, email: &email, expected: eff, configured, people };
    if configured.effective == Effective::DelegateOwner {
        // Ordinary scopes use tokeninfo only; the write probe ran separately.
        let client_id = match super::delegation_key::info(home) {
            Ok(Some(info)) => info.client_id,
            _ => None,
        };
        let Some(client_id) = client_id else {
            out.push(step("tokeninfo", "Google 權限", false, "無法確認服務帳號用戶端。"));
            return (Some(email), out);
        };
        for &(service, scopes) in super::delegated_scopes::SERVICES {
            if service == "chat" && !gw::chat_read_enabled(home) { continue; }
            for scope in scopes {
                let result: Result<(), String> = async {
                    let token = identity.mint(&[*scope]).await.map_err(|r| r.message_zh)?;
                    identity.verify().map_err(|r| r.message_zh)?;
                    let granted = crate::google_personal::oauth::tokeninfo(&token.access_token, &client_id).await
                        .map_err(|_| "無法確認 tokeninfo 權限或用戶端。".to_string())?;
                    identity.verify().map_err(|r| r.message_zh)?;
                    if !granted.iter().any(|s| s.as_str() == *scope) { return Err("Google 未回報要求的權限。".into()); }
                    Ok(())
                }.await;
                out.push(Step { id: format!("scope:{scope}"), label: "Google 權限", ok: result.is_ok(),
                    detail: result.err().unwrap_or_else(|| format!("{service}: {scope} — tokeninfo 已確認；未讀取資料。")) });
                if identity.verify().is_err() { return (Some(email), out); }
            }
        }
        return (Some(email), out);
    }

    // Each scope is independently requested so a missing Chat/Forms grant
    // cannot prevent diagnostics for already authorized services.
    for &(service, scopes) in super::delegated_scopes::SERVICES {
        if service == "chat" && !gw::chat_read_enabled(home) { continue; }
        for scope in scopes {
            let result = identity.mint(&[*scope]).await;
            out.push(Step { id: format!("scope:{scope}"), label: "Google 權限",
                ok: result.is_ok(), detail: match result {
                    Ok(_) => format!("{service}: {scope} — 已授權"),
                    Err(r) => format!("{service}: {scope} — 未確認授權：{}", r.message_zh),
                } });
        }
    }

    if let Ok(token) = identity.mint(&["https://www.googleapis.com/auth/gmail.readonly"]).await {
        match identity.get(&token.access_token, "https://gmail.googleapis.com/gmail/v1/users/me/profile").await {
            Ok(value) => {
                let matched = value.get("emailAddress").and_then(Value::as_str)
                    .and_then(|raw| gi::normalize_email(raw).ok()).as_deref() == Some(email.as_str());
                out.push(step("identity_match", "確認是這個人", matched,
                    if matched { "Google 回報的帳號與委派主體相同。" } else { "Google 回報的帳號與委派主體不符，已停止測試。" }));
                if !matched { return (Some(email), out); }
            }
            Err(_) => out.push(step("identity_match", "確認是這個人", false, "無法讀取 Gmail 帳號資料；其他服務會分別檢查。")),
        }
    }

    for &(id, label, scopes, probe) in SERVICES {
        let result = match identity.mint(scopes).await {
            Err(r) => Err(r.message_zh),
            Ok(t) => match probe {
                None => match identity.mint(&["https://www.googleapis.com/auth/drive.readonly"]).await {
                    Ok(discovery) => resource_probe(&identity, id, &discovery.access_token, &t.access_token).await,
                    Err(r) => Err(format!("檔案探索需要 Drive 權限：{}", r.message_zh)),
                },
                Some(url) => identity.get(&t.access_token, url)
                    .await
                    .map(|_| "可以使用".to_string())
                    .map_err(|e| {
                        format!(
                            "這個帳號可能沒有開通{label}：{}",
                            duduclaw_core::truncate_chars(&e.to_string(), 160)
                        )
                    }),
            },
        };
        out.push(match result {
            Ok(d) => step(id, label, true, d),
            Err(d) => step(id, label, false, d),
        });
    }
    if gw::chat_read_enabled(home) {
        let result = match identity.mint(gw::CHAT_READ_SCOPES).await {
            Ok(token) => identity.get(&token.access_token, "https://chat.googleapis.com/v1/spaces?pageSize=1")
                .await.map(|_| ()).map_err(|_| "Google Chat API 無法使用。".to_string()),
            Err(r) => Err(r.message_zh),
        };
        out.push(step("chat", "Google Chat", result.is_ok(), result.err().unwrap_or_else(|| "可以讀取聊天室清單。".into())));
    }
    (Some(email), out)
}
