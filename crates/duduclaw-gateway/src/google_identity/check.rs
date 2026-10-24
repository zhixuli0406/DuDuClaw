//! `google.identity.test {agent_id}`: prove a delegate works without sending
//! anything (design §7.3). Every step mints fresh (the cache is bypassed and
//! never written), reads at most one read-only endpoint, and reports one
//! line per check.

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

/// Per-service probe: scope group and one read-only endpoint (`None` =
/// minting with the scope is the check).
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
                None => Ok("已核准這項服務的授權；尚未讀取服務資料確認是否開通".to_string()),
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
