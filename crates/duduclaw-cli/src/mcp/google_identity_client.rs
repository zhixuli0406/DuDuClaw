//! Which Google account a native Google tool call uses (per-employee Google
//! identity, B2a).
//!
//! | caller's identity | path |
//! |---|---|
//! | `home` (and every employee without `[google]`) | unchanged: `resolve_backend(home)` in this process, no gateway round trip |
//! | `none` | refused (and the tools are not listed) |
//! | `delegate:<email>` | `POST /api/internal/google-token` on the gateway, which decides and mints; the token stays in this process's memory for this turn only |
//! | unreadable / not available | refused, never the home account |
//!
//! The caller is only ever the employee proven by `DUDUCLAW_AGENT_ID` +
//! `DUDUCLAW_AGENT_TOKEN` (or a per-agent key's own employee), never a tool
//! argument. A process that claims an employee it cannot prove may use only
//! the home default, and not even that when the claimed employee is set to
//! anything other than the home account.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use duduclaw_core::google_identity::{Effective, effective_for};
use duduclaw_gateway::google_workspace::{self as gw, GoogleBackend};
use serde_json::{Value, json};

const ROUTE: &str = "/api/internal/google-token";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[async_trait::async_trait]
impl duduclaw_gateway::google_identity::native_tools::NativeGoogleAccess for GoogleCaller {
    async fn backend(&self, home: &Path, tool: &str) -> Result<GoogleBackend, String> {
        GoogleCaller::backend(self, home, tool).await
    }

    async fn token(&self, home: &Path, tool: &str) -> Result<String, String> {
        GoogleCaller::token(self, home, tool).await
    }
}

/// Who is calling, as far as choosing a Google identity goes.
#[derive(Debug, Default)]
pub(crate) struct GoogleCaller {
    /// The proven employee (or a per-agent key's own employee).
    agent: Option<String>,
    /// An employee id the process claims but could not prove.
    unproven_claim: Option<String>,
    /// Dedicated calls execute in the host; no Google bearer reaches this process.
    dedicated: Option<crate::mcp_auth::AcceptedGoogleCall>,
    /// What the last acquisition used (for the audit row and the notice).
    last: Mutex<Option<CallRecord>>,
}

#[derive(Debug, Clone)]
struct CallRecord {
    identity: &'static str,
    account: String,
    fallback: bool,
    notice: Option<String>,
    user_id: Option<String>,
}

/// How a call will get its credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    Home,
    Gateway { agent: String },
    Refuse(String),
}

impl GoogleCaller {
    /// The caller of this MCP process for `caller_client_id`.
    pub(crate) fn for_process(home: &Path, caller_client_id: &str) -> Self {
        if let Some(call) = crate::mcp_auth::current_accepted_google_call() {
            // Only the lane accepted by the dispatcher is sticky; merely
            // authenticating an internal integration key does not change Home.
            return Self {
                agent: (call.principal().client_id == caller_client_id)
                    .then(|| call.principal().client_id.clone()),
                dedicated: Some(call),
                ..Default::default()
            };
        }
        if let Some(a) = crate::mcp_namespace::verified_employee_from_env(home) {
            return Self {
                agent: Some(a),
                ..Default::default()
            };
        }
        let internal = caller_client_id.is_empty()
            || caller_client_id == duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
        if !internal && crate::mcp_namespace::client_is_agent(home, caller_client_id) {
            return Self {
                agent: Some(caller_client_id.to_string()),
                ..Default::default()
            };
        }
        let claim = std::env::var(duduclaw_core::ENV_AGENT_ID)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| duduclaw_core::is_valid_agent_id(v));
        Self {
            agent: None,
            unproven_claim: claim,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn test(agent: Option<&str>, unproven_claim: Option<&str>) -> Self {
        Self {
            agent: agent.map(str::to_string),
            unproven_claim: unproven_claim.map(str::to_string),
            ..Default::default()
        }
    }

    pub(crate) fn is_dedicated(&self) -> bool {
        self.dedicated.is_some()
    }

    fn dedicated_authority(
        &self,
        home: &Path,
        tool: &str,
        args: &Value,
    ) -> Result<(&str, &duduclaw_core::google_identity::CredentialRef), &'static str> {
        let refused = "Google 專用連線已失效或權限已變更，這次操作尚未執行。請重新連線後再試。";
        let call = self.dedicated.as_ref().ok_or(refused)?;
        let agent = self.agent.as_deref().ok_or(refused)?;
        if agent != call.principal().client_id
            || !call.matches(tool, args)
            || !crate::mcp_auth::dedicated_google_tool_allowed(home, call.principal(), tool)
        {
            return Err(refused);
        }
        Ok((agent, call.credential_ref().ok_or(refused)?))
    }

    pub(crate) async fn execute_dedicated(&self, home: &Path, tool: &str, args: &Value) -> Value {
        let (agent, reference) = match self.dedicated_authority(home, tool, args) {
            Ok(authority) => authority,
            Err(error) => return super::tool_error(error),
        };
        match super::google_execute_client::execute(
            home,
            agent,
            reference,
            tool,
            args,
            self.dedicated.as_ref().and_then(|c| c.selection_stamp()),
        )
        .await
        {
            Ok(result) => {
                if let Some(selection) = self.dedicated.as_ref().and_then(|c| c.selection_stamp()) {
                    use duduclaw_core::google_identity::ApprovedGoogleIdentity;
                    let personal = selection.approval.selected_identity == ApprovedGoogleIdentity::Personal;
                    self.record(CallRecord {
                        identity: if personal { "personal" } else { "home" },
                        account: String::new(),
                        user_id: personal.then(|| selection.approval.selected_credential_owner.clone()),
                        fallback: !personal,
                        notice: None,
                    });
                } else if matches!(effective_for(home, Some(agent)).effective, Effective::Home) {
                    self.record(CallRecord {
                        identity: "home", account: String::new(), user_id: None,
                        fallback: false, notice: None,
                    });
                } else {
                    // Legacy delegated execution can fall back inside the host.
                    // Without its selected-account receipt no exact stamp is safe.
                    let _ = duduclaw_gateway::egress_client::mark_connector(home, agent, tool, "unknown").await;
                }
                result
            },
            Err(error) => super::tool_error(&error),
        }
    }

    /// The decision for this caller (local files only).
    pub(crate) fn plan(&self, home: &Path) -> Plan {
        if let Some(a) = &self.agent {
            return plan_of(&effective_for(home, Some(a)).effective, a);
        }
        if let Some(claim) = &self.unproven_claim
            && effective_for(home, Some(claim)).effective != Effective::Home
        {
            return Plan::Refuse(
                "無法確認這個呼叫來自哪位 AI 員工（員工身分驗證沒有通過），所以不能使用這位員工設定的 Google 帳號。請由 DuDuClaw 重新啟動這位員工。".into(),
            );
        }
        plan_of(&effective_for(home, None).effective, "")
    }

    /// Whether the Google tools are listed for this caller (only an
    /// effective `none` hides them).
    pub(crate) fn tools_listed(&self, home: &Path) -> bool {
        let eff = match (&self.agent, &self.unproven_claim) {
            (Some(a), _) => effective_for(home, Some(a)).effective,
            (None, _) => effective_for(home, None).effective,
        };
        eff.tools_listed()
    }

    fn record(&self, r: CallRecord) {
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
    }

    /// A bearer backend for a tool that supports the Apps Script bridge too.
    pub(crate) async fn backend(&self, home: &Path, tool: &str) -> Result<GoogleBackend, String> {
        if self.is_dedicated() {
            return Err("Google 專用連線必須由 DuDuClaw 主程式執行，不能取得 Google 權杖。".into());
        }
        match self.plan(home) {
            Plan::Home => {
                self.record(CallRecord {
                    identity: "home",
                    account: String::new(),
                    user_id: None,
                    fallback: false,
                    notice: None,
                });
                gw::resolve_backend(home).await.map_err(local_google_auth_error)
            }
            Plan::Refuse(m) => {
                duduclaw_gateway::egress_client::builtin_not_dispatched();
                self.record(CallRecord {
                    identity: "refused",
                    account: String::new(),
                    user_id: None,
                    fallback: false,
                    notice: None,
                });
                Err(m)
            }
            Plan::Gateway { agent } => match fetch(home, &agent, tool).await {
                Ok(Fetched::Token {
                    token,
                    account,
                    kind,
                    user_id,
                }) => {
                    self.record(CallRecord {
                        identity: kind,
                        account,
                        user_id,
                        fallback: false,
                        notice: None,
                    });
                    Ok(GoogleBackend::Direct(token))
                }
                Ok(Fetched::Home { fallback, notice }) => {
                    self.record(CallRecord {
                        identity: "home",
                        account: String::new(),
                        user_id: None,
                        fallback,
                        notice,
                    });
                    gw::resolve_backend(home).await.map_err(local_google_auth_error)
                }
                Err(m) => {
                    self.record(CallRecord {
                        identity: "refused",
                        account: String::new(),
                        user_id: None,
                        fallback: false,
                        notice: None,
                    });
                    Err(m)
                }
            },
        }
    }

    /// A bearer token for a tool that needs the direct API.
    pub(crate) async fn token(&self, home: &Path, tool: &str) -> Result<String, String> {
        if self.is_dedicated() {
            return Err("Google 專用連線必須由 DuDuClaw 主程式執行，不能取得 Google 權杖。".into());
        }
        match self.plan(home) {
            Plan::Home => {
                self.record(CallRecord {
                    identity: "home",
                    account: String::new(),
                    user_id: None,
                    fallback: false,
                    notice: None,
                });
                gw::get_valid_google_token(home)
                    .await
                    .map_err(local_google_auth_error)
            }
            _ => match self.backend(home, tool).await? {
                GoogleBackend::Direct(t) => Ok(t),
                GoogleBackend::AppsScript(_) => gw::get_valid_google_token(home)
                    .await
                    .map_err(local_google_auth_error),
            },
        }
    }

    /// After a Google tool ran: prepend a fallback notice to the result and
    /// write the audit row for a person-identity path (the home path keeps
    /// today's audit exactly).
    pub(crate) async fn finish(
        &self,
        home: &Path,
        audit_agent: &str,
        tool: &str,
        mut result: Value,
    ) -> Value {
        let Some(rec) = self.last.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return result;
        };
        if !result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            let reference = match (rec.identity, rec.user_id.as_deref()) {
                ("home", _) => Some("google:home".to_string()),
                ("delegate", _) => Some("google:delegation".to_string()),
                ("personal" | "owner" | "user", Some(user)) => Some(format!("google:person:{user}")),
                _ => None,
            };
            if let Some(reference) = reference {
                if let Err(reason) = duduclaw_gateway::egress_client::mark_connector(
                    home, self.agent.as_deref().unwrap_or(audit_agent), tool, &reference,
                ).await {
                    tracing::warn!(%reason, "Google connection attribution unavailable");
                }
            }
        }
        if let Some(n) = &rec.notice
            && let Some(first) = result
                .get_mut("content")
                .and_then(|c| c.get_mut(0))
                .and_then(|c| c.get_mut("text"))
            && let Some(text) = first.as_str()
        {
            *first = json!(format!("（{n}）\n{text}"));
        }
        if rec.identity != "home" || rec.fallback {
            let success = !result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            duduclaw_security::audit::append_tool_call_with_extras(
                home,
                self.agent.as_deref().unwrap_or(audit_agent),
                "google_call",
                &format!("tool={tool} identity={}", rec.identity),
                success,
                &[
                    ("google_tool", json!(tool)),
                    ("google_identity", json!(rec.identity)),
                    ("google_account", json!(rec.account)),
                    ("google_identity_user", json!(rec.user_id)),
                    ("fallback", json!(rec.fallback)),
                ],
            );
        }
        result
    }
}

fn plan_of(eff: &Effective, agent: &str) -> Plan {
    match eff {
        Effective::Home => Plan::Home,
        Effective::Delegate { .. } | Effective::Personal { .. } if !agent.is_empty() => {
            Plan::Gateway {
                agent: agent.to_string(),
            }
        }
        other => Plan::Refuse(
            duduclaw_gateway::google_identity::resolve::refusal_for(other)
                .map(|r| r.message_zh)
                .unwrap_or_else(|| "這位 AI 員工的 Google 身分設定有誤。".into()),
        ),
    }
}

enum Fetched {
    Token {
        token: String,
        account: String,
        kind: &'static str,
        user_id: Option<String>,
    },
    Home {
        fallback: bool,
        notice: Option<String>,
    },
}

fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
}

async fn fetch(home: &Path, agent: &str, tool: &str) -> Result<Fetched, String> {
    let turn_id = env_trimmed(duduclaw_core::ENV_TRUST_TURN_ID);
    let run_id = env_trimmed(duduclaw_core::ENV_DISPATCH_RUN_ID);
    // Re-authorize every call, including within one turn: the selector,
    // requester bindings, account status and protection may have changed.
    // The gateway caches minting only, after checking those gates again.
    let body = json!({ "tool": tool, "turn_id": turn_id, "run_id": run_id });
    let v = post(home, agent, &body).await?;
    match v.get("mode").and_then(Value::as_str) {
        Some("delegate") => {
            let token = v
                .get("access_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let account = v
                .get("account_email")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if token.is_empty() {
                return Err("DuDuClaw gateway 的回應無法辨識。".into());
            }
            let kind = match v.get("identity_kind").and_then(Value::as_str) {
                Some("owner") => "owner",
                Some("user") => "user",
                _ => "delegate",
            };
            let user_id = v
                .get("google_identity_user")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(Fetched::Token {
                token,
                account,
                kind,
                user_id,
            })
        }
        Some("home") => Ok(Fetched::Home {
            fallback: v.get("fallback").and_then(Value::as_bool).unwrap_or(false),
            notice: v
                .get("notice")
                .and_then(Value::as_str)
                .map(|s| duduclaw_core::truncate_chars(s, 500)),
        }),
        _ => Err("DuDuClaw gateway 的回應無法辨識。".into()),
    }
}

const UNREACHABLE: &str = "Google 帳號暫時無法使用：DuDuClaw 主程式沒有回應。這位 AI 員工代表特定的人使用 Google，必須經過主程式，請確認 DuDuClaw 正在執行後再試。";

async fn post(home: &Path, agent: &str, body: &Value) -> Result<Value, String> {
    let key = env_trimmed(duduclaw_core::ENV_MCP_API_KEY)
        .ok_or("這位 AI 員工代表特定的人使用 Google，只有由 DuDuClaw 啟動的 AI 員工能使用（缺少主程式的內部金鑰）。")?;
    let token = env_trimmed(duduclaw_core::ENV_AGENT_TOKEN)
        .ok_or("缺少員工身分權杖，請由 DuDuClaw 重新啟動這位員工。")?;
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| UNREACHABLE.to_string())?;
    let bytes = serde_json::to_vec(body).map_err(|_| UNREACHABLE.to_string())?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nonce: [u8; 16] = rand::random();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}{ROUTE}"))
        .header("Content-Type", "application/json");
    for (name, value) in
        super::computer_use_client::signed_headers(&key, agent, &token, timestamp, &nonce, &bytes)
    {
        request = request.header(name, value);
    }
    duduclaw_gateway::egress_client::builtin_dispatch_started();
    let mut resp = request
        .body(bytes)
        .send()
        .await
        .map_err(|_| UNREACHABLE.to_string())?;
    let status = resp.status().as_u16();
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| UNREACHABLE.to_string())? {
        if out.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("DuDuClaw gateway 的回應過大，已捨棄。".into());
        }
        out.extend_from_slice(&chunk);
    }
    interpret(status, &out)
}

fn interpret(status: u16, body: &[u8]) -> Result<Value, String> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return Err(format!(
            "DuDuClaw gateway 的回應無法辨識（HTTP {status}）。"
        ));
    };
    if status == 200 && v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v);
    }
    match v.get("message").and_then(Value::as_str).map(str::trim) {
        Some(m) if !m.is_empty() => Err(duduclaw_core::truncate_chars(m, 500)),
        _ => Err(format!(
            "DuDuClaw gateway 拒絕了這個 Google 請求（HTTP {status}）。"
        )),
    }
}

/// `google_status`'s first lines: who the employee acts as, and (for a
/// person identity) whether it can be used right now.
pub(crate) async fn status_header(home: &Path, caller: &GoogleCaller) -> (String, bool) {
    match caller.plan(home) {
        Plan::Home => ("目前代表：公司共用帳號（home）。\n".into(), true),
        Plan::Refuse(m) => (format!("目前代表：無法使用 — {m}\n"), false),
        Plan::Gateway { agent } => {
            let configured = match effective_for(home, Some(&agent)).effective {
                Effective::Delegate { email } => format!("公司授權代表 {email}（網域委派）"),
                Effective::Personal { selector } => match selector {
                    duduclaw_core::google_identity::Selector::Owner => {
                        "跟著負責人的 Google 帳號".into()
                    }
                    _ => "指定同事的 Google 帳號".into(),
                },
                _ => "設定已變更".into(),
            };
            let (identity, state) = match fetch(home, &agent, "google_status").await {
                Ok(Fetched::Token { account, kind, .. }) => (
                    if kind == "delegate" {
                        configured
                    } else {
                        format!("個人 Google 帳號 {account}")
                    },
                    "已連線；各工具仍依本人實際開放的服務判斷".to_string(),
                ),
                Ok(Fetched::Home { notice, .. }) => (
                    "公司共用帳號".into(),
                    notice.unwrap_or_else(|| "改用公司共用帳號".into()),
                ),
                Err(m) => (configured, format!("目前不能使用：{m}")),
            };
            (format!("目前代表：{identity}。\n狀態：{state}\n"), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dedicated_receipt_keeps_its_lane_when_employee_config_becomes_invalid() {
        use crate::mcp_auth::{authenticate_with_key_call, with_authenticated_call};
        for removed in [true, false] {
            let home = home_with("bot", "[google]\nidentity='none'\n");
            let key = "ddc_dev_0123456789abcdef0123456789abcdef";
            std::fs::write(home.path().join("config.toml"), format!(
                "[mcp_keys.{key}]\nclient_id='bot'\nscopes=['google:read']\nis_external=true\ncreated_at='{}'\n",
                chrono::Utc::now().to_rfc3339(),
            )).unwrap();
            let call = authenticate_with_key_call(key, home.path()).unwrap();
            with_authenticated_call(call, async {
                let args = json!({"query":"unit-fixed"});
                let current = crate::mcp_auth::current_authenticated_call().unwrap();
                let accepted = crate::mcp_auth::accept_google_lane(
                    home.path(),
                    current.principal(),
                    "gmail_search",
                )
                .unwrap()
                .bind(args.clone());
                crate::mcp_auth::with_accepted_google_call(Some(accepted), async {
                    let initial = GoogleCaller::for_process(home.path(), "bot");
                    assert!(initial.is_dedicated());
                    assert!(
                        initial
                            .dedicated_authority(home.path(), "gmail_search", &args)
                            .is_ok()
                    );
                    assert!(
                        initial
                            .dedicated_authority(home.path(), "gmail_create_draft", &args)
                            .is_err()
                    );
                    assert!(
                        initial
                            .dedicated_authority(
                                home.path(),
                                "gmail_search",
                                &json!({"query":"changed"})
                            )
                            .is_err()
                    );
                    let path = home.path().join("agents/bot/agent.toml");
                    if removed {
                        std::fs::remove_file(path).unwrap();
                    } else {
                        std::fs::write(path, "[capabilities]\nallowed_tools=[").unwrap();
                    }
                    let caller = GoogleCaller::for_process(home.path(), "bot");
                    assert!(
                        caller.is_dedicated(),
                        "an invalid employee must not change the receipt's lane"
                    );
                    assert_eq!(caller.agent.as_deref(), Some("bot"));
                    assert!(
                        caller
                            .dedicated_authority(home.path(), "gmail_search", &args)
                            .is_err()
                    );
                    // These methods must refuse before any vault, network or
                    // ambient identity lookup; no account credentials exist here.
                    assert!(caller.token(home.path(), "gmail_search").await.is_err());
                    assert!(caller.backend(home.path(), "gmail_search").await.is_err());
                })
                .await;
            })
            .await;
        }
    }

    #[tokio::test]
    async fn internal_custom_key_without_employee_keeps_original_home_path() {
        use crate::mcp_auth::{authenticate_with_key_call, with_authenticated_call};
        let home = tempfile::tempdir().unwrap();
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        std::fs::write(home.path().join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='integration-client'\nscopes=['google:read']\nis_external=false\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let call = authenticate_with_key_call(key, home.path()).unwrap();
        with_authenticated_call(call, async {
            let current = crate::mcp_auth::current_authenticated_call().unwrap();
            assert!(
                crate::mcp_auth::accept_google_lane(
                    home.path(),
                    current.principal(),
                    "gmail_search"
                )
                .is_none()
            );
            let caller = GoogleCaller::for_process(home.path(), "integration-client");
            assert!(!caller.is_dedicated());
            assert_eq!(caller.plan(home.path()), Plan::Home);
        })
        .await;
        let scope = crate::mcp_auth::with_accepted_google_call(None, std::future::pending::<()>());
        assert!(std::mem::size_of_val(&scope) <= 512);
    }

    fn home_with(agent: &str, google: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname='{agent}'\n{google}"),
        )
        .unwrap();
        home
    }

    #[test]
    fn plans_follow_the_verified_caller_only() {
        let home = home_with("bot", "[google]\nidentity='delegate:a@c.test'\n");
        let p = home.path();
        assert_eq!(
            GoogleCaller::test(Some("bot"), None).plan(p),
            Plan::Gateway {
                agent: "bot".into()
            }
        );
        // Anonymous: the home default.
        assert_eq!(GoogleCaller::test(None, None).plan(p), Plan::Home);
        // Claiming the delegate without proof: refused, not home.
        assert!(matches!(
            GoogleCaller::test(None, Some("bot")).plan(p),
            Plan::Refuse(_)
        ));
        // Claiming an employee on the home account: the home default.
        std::fs::create_dir_all(p.join("agents/plain")).unwrap();
        std::fs::write(p.join("agents/plain/agent.toml"), "[agent]\nname='plain'\n").unwrap();
        assert_eq!(GoogleCaller::test(None, Some("plain")).plan(p), Plan::Home);
    }

    #[test]
    fn none_hides_and_refuses_and_invalid_refuses() {
        let home = home_with("off", "[google]\nidentity='none'\n");
        let c = GoogleCaller::test(Some("off"), None);
        assert!(!c.tools_listed(home.path()));
        assert!(matches!(c.plan(home.path()), Plan::Refuse(_)));
        std::fs::write(
            home.path().join("agents/off/agent.toml"),
            "[agent]\nname='off'\n[google]\nidentity='bogus'\n",
        )
        .unwrap();
        assert!(
            c.tools_listed(home.path()),
            "a broken setting stays listed so the call can explain"
        );
        assert!(matches!(c.plan(home.path()), Plan::Refuse(m) if m.contains("設定有誤")));
    }

    #[tokio::test]
    async fn person_identity_without_the_gateway_is_refused_not_home() {
        let home = home_with("bot", "[google]\nidentity='delegate:a@c.test'\n");
        let c = GoogleCaller::test(Some("bot"), None);
        // No internal key in this test process: refused before any network.
        let Err(err) = c.backend(home.path(), "gmail_search").await else {
            panic!("a person identity must not resolve without the gateway");
        };
        assert!(err.contains("DuDuClaw"), "{err}");
        let out = c.finish(
            home.path(),
            "bot",
            "gmail_search",
            json!({"content":[{"type":"text","text":err}],"isError":true}),
        ).await;
        assert_eq!(out["isError"], true);
        let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        assert!(audit.contains("\"google_identity\":\"refused\""), "{audit}");
    }

    #[tokio::test]
    async fn fallback_notice_is_prepended() {
        let home = tempfile::tempdir().unwrap();
        let c = GoogleCaller::test(Some("bot"), None);
        c.record(CallRecord {
            identity: "home",
            account: String::new(),
            user_id: None,
            fallback: true,
            notice: Some("已改用公司共用帳號處理".into()),
        });
        let out = c.finish(
            home.path(),
            "bot",
            "gmail_search",
            json!({"content":[{"type":"text","text":"結果"}]}),
        ).await;
        assert_eq!(
            out["content"][0]["text"],
            "（已改用公司共用帳號處理）\n結果"
        );
    }

    #[test]
    fn gateway_answers_are_interpreted() {
        assert!(interpret(200, br#"{"ok":true,"mode":"home"}"#).is_ok());
        let e = interpret(
            403,
            r#"{"ok":false,"code":"requester_refused","message":"這個對話的發問者沒有權限"}"#
                .as_bytes(),
        )
        .unwrap_err();
        assert!(e.contains("沒有權限"));
        assert!(interpret(502, b"<html>").is_err());
    }
}

fn local_google_auth_error(error: duduclaw_gateway::google_workspace::GoogleAuthError) -> String {
    use duduclaw_gateway::google_workspace::GoogleAuthError;
    if matches!(&error, GoogleAuthError::NotConnected { .. } | GoogleAuthError::NoRefreshToken | GoogleAuthError::ClientConfigMissing) {
        duduclaw_gateway::egress_client::builtin_not_dispatched();
    }
    error.to_string()
}
