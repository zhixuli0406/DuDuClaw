//! Legacy Home/delegate authority for `oauth://google` mounts. Personal HTTP
//! mounts use the opaque owned Closed operation exported below, with no bearer
//! outlet to the generic MCP client. Every call captures its real requester.
use std::path::{Path, PathBuf};

use duduclaw_core::google_identity::{Effective, Fallback, effective_for};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_llm::{InterceptDecision, ToolInterceptor};
use serde_json::Value;

use super::{Refusal, delegation_key, requester_gate, resolve};

#[derive(Debug, Clone)]
pub(crate) struct GoogleMountGuard {
    home: PathBuf,
    agent: String,
    selected: Effective,
    fallback: bool,
    notice: Option<String>,
}

/// This is host task-local authority, not environment or wire input. A
/// dispatched run is constrained by its gateway registration/upstream person.
pub(in crate::google_identity) fn requester(agent: &str) -> RequesterIdentity {
    let run = crate::memory_provenance::DISPATCH_RUN
        .try_with(|run| run.as_ref().map(|(_, key)| key.clone()))
        .ok()
        .flatten();
    if let Some(run) = run {
        crate::knowledge::turn_requesters::resolve_with_run(agent, None, Some(&run))
    } else {
        crate::knowledge::turn_requesters::current_requester()
    }
}

impl GoogleMountGuard {
    /// Only the canonical employee directory selected by the gateway can
    /// prepare a person mount. An arbitrary path cannot nominate an employee.
    pub(crate) async fn prepare(home: &Path, agent_dir: &Path) -> Result<(String, Self), Refusal> {
        let home = home.canonicalize().map_err(|_| unavailable())?;
        let agents = home
            .join("agents")
            .canonicalize()
            .map_err(|_| unavailable())?;
        let dir = agent_dir.canonicalize().map_err(|_| unavailable())?;
        let agent = dir
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|id| duduclaw_core::is_valid_agent_id(id))
            .ok_or_else(unavailable)?
            .to_string();
        if dir.parent() != Some(agents.as_path()) || !dir.join("agent.toml").is_file() {
            return Err(unavailable());
        }
        let effective = effective_for(&home, Some(&agent));
        let who = requester(&agent);
        let resolved = match &effective.effective {
            Effective::Home => resolve::Resolved::UseHome {
                fallback: false,
                notice: None,
            },
            Effective::Delegate { .. } => {
                if !crate::google_workspace::integration_enabled(&home) {
                    return Err(Refusal::hard("disabled", "Google 整合尚未開啟。"));
                }
                let people = requester_gate::people_for(&home, None);
                resolve::resolve_person(&home, &agent, &who, people.as_ref()).await?
            }
            other => return Err(resolve::refusal_for(other).unwrap_or_else(unavailable)),
        };
        let (token, fallback, notice) = match resolved {
            resolve::Resolved::Delegated { token, email } => (
                token.access_token,
                false,
                Some(format!("目前代表 {email} 使用 Google。")),
            ),
            resolve::Resolved::Personal { .. } => return Err(unavailable()),
            resolve::Resolved::UseHome { fallback, notice } => (
                crate::google_workspace::get_valid_google_token(&home)
                    .await
                    .map_err(|_| {
                        Refusal::hard("account_unavailable", "公司共用 Google 帳號暫時無法使用。")
                    })?,
                fallback,
                notice,
            ),
        };
        let guard = Self {
            home,
            agent,
            selected: effective.effective,
            fallback,
            notice,
        };
        let check = guard.clone();
        tokio::task::spawn_blocking(move || check.check(&who))
            .await
            .map_err(|_| unavailable())??;
        Ok((token, guard))
    }

    /// A host annotation in the actual tool descriptions informs the model
    /// before use, without changing JSON result paths before redaction/CCR.
    pub(crate) fn account_notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub(crate) fn is_person_identity(&self) -> bool {
        self.selected != Effective::Home
    }
    pub(crate) fn fallback_notice(&self) -> Option<&str> {
        self.fallback.then_some(self.notice.as_deref()).flatten()
    }

    fn check(&self, who: &RequesterIdentity) -> Result<(), Refusal> {
        let dir = self.home.join("agents").join(&self.agent);
        if !dir.join("agent.toml").is_file() {
            return Err(unavailable());
        }
        let now = effective_for(&self.home, Some(&self.agent));
        if now.effective != self.selected || (self.fallback && now.fallback != Fallback::Home) {
            return Err(Refusal::hard(
                "identity_changed",
                "這位 AI 員工的 Google 帳號設定已變更，請重新開始這輪工作後再試。",
            ));
        }
        match &now.effective {
            Effective::Home => Ok(()),
            Effective::Delegate { email } => {
                if !crate::google_workspace::integration_enabled(&self.home) {
                    return Err(Refusal::hard("disabled", "Google 整合尚未開啟。"));
                }
                let people = requester_gate::people_for(&self.home, None);
                requester_gate::check(now.requesters, who, &self.agent, email, people.as_ref())?;
                if let Err(error) = resolve::precheck(&self.home, email, people.as_ref()) {
                    if !self.fallback || !error.fallback_eligible {
                        return Err(error);
                    }
                }
                let protected = super::protection_gate(&self.home)?;
                if let Err(error) = delegation_key::load(&self.home, &protected) {
                    if !self.fallback || !error.fallback_eligible {
                        return Err(error);
                    }
                }
                Ok(())
            }
            _ => Err(unavailable()),
        }
    }
}

fn unavailable() -> Refusal {
    Refusal::hard(
        "no_verified_caller",
        "無法確認這個 Google 外掛連線目前所代表的員工與帳號。",
    )
}

#[async_trait::async_trait]
impl ToolInterceptor for GoogleMountGuard {
    fn before_call(&self, _: &str, _: &str, args: Value) -> InterceptDecision {
        match self.check(&requester(&self.agent)) {
            Ok(()) => InterceptDecision::Allow(args),
            Err(error) => InterceptDecision::Deny(error.message_zh),
        }
    }
    async fn before_call_async(&self, _: &str, _: &str, args: Value) -> InterceptDecision {
        // Capture host authority BEFORE crossing to a thread without task locals.
        let who = requester(&self.agent);
        let guard = self.clone();
        match tokio::task::spawn_blocking(move || guard.check(&who)).await {
            Ok(Ok(())) => InterceptDecision::Allow(args),
            Ok(Err(error)) => InterceptDecision::Deny(error.message_zh),
            Err(_) => {
                InterceptDecision::Deny("Google 外掛連線的權限無法確認，已拒絕本次呼叫。".into())
            }
        }
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {}
}

pub(crate) use super::effect_authority::GoogleClosedMount;
