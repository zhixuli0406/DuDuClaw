//! `google.identity.test {agent_id}`: prove a delegate works without sending
//! anything (design §7.3). Every step mints fresh (the cache is bypassed and
//! never written), reads only GET endpoints, and reports one line per check.
//! Document services use a Drive metadata lookup and then their own API.

use std::path::Path;

use duduclaw_core::google_identity::{self as gi, Effective};
use serde::Serialize;
use serde_json::Value;

use super::requester_gate::People;
use super::resolve::{mint_delegate, precheck, refusal_for};
use crate::google_service_account::CacheMode;
use crate::google_workspace::{self as gw, REQUIRED_SCOPES};

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub id: &'static str,
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
        id,
        label,
        ok,
        detail: detail.into(),
    }
}

/// Only the resource id crosses this boundary; no document content, title or
/// sample id is returned to the dashboard. No sample means unverified, never
/// that issuing a token proved the service's data API works.
async fn resource_probe(
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
    let files = gw::api_get(discovery_token, lookup.as_str())
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
    let result = gw::api_get(
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

/// Run the checks for `agent`. Stops at the first check later ones depend on.
pub async fn run(home: &Path, agent: &str, people: &dyn People) -> (Option<String>, Vec<Step>) {
    let mut out = Vec::new();
    let eff = gi::effective_for(home, Some(agent));
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

    let full = match mint_delegate(home, &email, REQUIRED_SCOPES, CacheMode::Bypass).await {
        Ok(t) => {
            out.push(step(
                "authorize",
                "授權清單完整",
                true,
                "用全部權限向 Google 取得權杖成功",
            ));
            t
        }
        Err(r) => {
            let id = if r.code == "credential_protection" || r.code == "delegate_unavailable" {
                "key"
            } else {
                "authorize"
            };
            let label = if id == "key" {
                "服務帳號金鑰"
            } else {
                "授權清單完整"
            };
            out.push(step(id, label, false, r.message_zh));
            return (Some(email), out);
        }
    };

    match gw::api_get(
        &full.access_token,
        "https://gmail.googleapis.com/gmail/v1/users/me/profile",
    )
    .await
    {
        Ok(v) => {
            let got = v
                .get("emailAddress")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if got == email {
                out.push(step(
                    "identity_match",
                    "確認是這個人",
                    true,
                    format!("Google 回報的帳號是 {email}"),
                ));
            } else {
                out.push(step(
                    "identity_match",
                    "確認是這個人",
                    false,
                    format!(
                        "Google 回報的帳號（{}）和設定的 {email} 不一致，已停止。",
                        duduclaw_core::truncate_chars(&got, 120)
                    ),
                ));
                return (Some(email), out);
            }
        }
        Err(e) => {
            out.push(step(
                "identity_match",
                "確認是這個人",
                false,
                format!(
                    "讀取 Gmail 帳號資料失敗：{}",
                    duduclaw_core::truncate_chars(&e.to_string(), 200)
                ),
            ));
        }
    }

    for &(id, label, scopes, probe) in SERVICES {
        let result = match mint_delegate(home, &email, scopes, CacheMode::Bypass).await {
            Err(r) => Err(r.message_zh),
            Ok(t) => match probe {
                None => resource_probe(id, &full.access_token, &t.access_token).await,
                Some(url) => gw::api_get(&t.access_token, url)
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
    (Some(email), out)
}
