//! Internal Google onboarding writer. No RPC parameters can construct authority.
use duduclaw_auth::{avatar::AvatarActor, UserContext};
use serde_json::{json, Value};

use super::config;
use crate::{handlers::MethodHandler, protocol::WsFrame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SigninAvatarError {
    ProtectedMode,
    TemplateChanged,
    TemplateInvalid,
    SeatsFull,
    NameTaken,
    PolicyChanged,
    NotEligible,
    Failed,
}

pub(crate) struct SigninAvatarRequest {
    pub member_id: String,
    pub agent_name: String,
    pub display_name: String,
    pub template: config::AvatarTemplate,
    pub settings_revision: String,
}

/// A Rust-only capability passed to the shared writers. In particular this
/// must never implement Deserialize or be selected by inspecting request JSON.
#[derive(Clone, Copy)]
pub(crate) enum AvatarCreation<'a> {
    None,
    Admin { member: &'a str, ctx: &'a UserContext },
    SigninPolicy {
        member: &'a str,
        revision: &'a str,
        // The capability crosses awaits, unlike auth's synchronous actor.
        policy_check: &'a (dyn Fn() -> Result<(), String> + Sync),
    },
}

impl<'a> AvatarCreation<'a> {
    pub(crate) fn member(self) -> Option<&'a str> {
        match self {
            Self::None => None,
            Self::Admin { member, .. } | Self::SigninPolicy { member, .. } => Some(member),
        }
    }

    pub(crate) fn is_signin(self) -> bool { matches!(self, Self::SigninPolicy { .. }) }

    /// Construct auth's non-Sync actor only in the synchronous publish step.
    pub(crate) fn actor(self) -> Option<AvatarActor<'a>> {
        match self {
            Self::None => None,
            Self::Admin { ctx, .. } => Some(AvatarActor::Admin(&ctx.user_id)),
            Self::SigninPolicy { revision, policy_check, .. } =>
                Some(AvatarActor::SigninPolicy { revision, policy_check }),
        }
    }

    /// Publication can fail either before or after rename. A private marker
    /// lets compensation distinguish our tree from a competing directory.
    pub(crate) fn publication(self, stage: &std::path::Path, target: &std::path::Path)
        -> std::io::Result<Option<SigninPublication>> {
        if !self.is_signin() { return Ok(None); }
        let marker = format!(".signin-publication-{}", uuid::Uuid::new_v4());
        std::fs::OpenOptions::new().write(true).create_new(true).open(stage.join(&marker))?;
        Ok(Some(SigninPublication { target: target.to_path_buf(), marker }))
    }
}

pub(crate) struct SigninPublication {
    target: std::path::PathBuf,
    marker: String,
}

impl SigninPublication {
    pub(crate) fn published(&self) -> bool {
        std::fs::symlink_metadata(&self.target).is_ok_and(|meta| meta.is_dir())
            && std::fs::symlink_metadata(self.target.join(&self.marker)).is_ok_and(|meta| meta.is_file())
    }
}

impl Drop for SigninPublication {
    fn drop(&mut self) {
        if self.published() {
            if let Err(error) = std::fs::remove_file(self.target.join(&self.marker)) {
                tracing::warn!(error = %error, "signin avatar publication marker cleanup failed");
            }
        }
    }
}

const POLICY_CHANGED: &str = "signin_avatar_policy_changed";
const TEMPLATE_CHANGED: &str = "signin_avatar_template_changed";
const TEMPLATE_INVALID: &str = "signin_avatar_template_invalid";

impl MethodHandler {
    pub(crate) async fn create_signin_avatar(&self, req: SigninAvatarRequest) -> Result<(), SigninAvatarError> {
        if crate::runtime_readiness::isolation_active() {
            return Err(SigninAvatarError::ProtectedMode);
        }
        let mut template = match &req.template {
            config::AvatarTemplate::Invalid => return Err(SigninAvatarError::TemplateInvalid),
            config::AvatarTemplate::Blank => None,
            config::AvatarTemplate::Company { key, revision } => {
                let item = crate::company_templates::get(self.home_dir(), key)
                    .map_err(|_| SigninAvatarError::TemplateInvalid)?;
                if item.premium || !item.members.is_empty() {
                    return Err(SigninAvatarError::TemplateInvalid);
                }
                if item.revision != *revision {
                    return Err(SigninAvatarError::TemplateChanged);
                }
                Some(item)
            }
        };
        if !duduclaw_core::is_valid_agent_id(&req.agent_name)
            || duduclaw_core::is_reserved_agent_id(&req.agent_name) {
            return Err(SigninAvatarError::Failed);
        }
        // Includes empty directories and dangling symlinks. Never adopt or
        // delete leftovers from an earlier attempt; P11 chooses a new name.
        let target = self.registry().read().await.agents_dir().join(&req.agent_name);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => return Err(SigninAvatarError::NameTaken),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err(SigninAvatarError::Failed),
        }
        let bindings = duduclaw_core::preset::load_bindings_checked(self.home_dir())
            .map_err(|_| SigninAvatarError::Failed)?;
        if bindings.contains(&req.agent_name) { return Err(SigninAvatarError::NameTaken); }

        let clean: String = req.display_name.chars().filter(|c| !c.is_control()).collect();
        let display_name = duduclaw_core::truncate_chars(&clean, 200);
        let params = json!({"name":req.agent_name,"display_name":display_name,
            "role":"specialist","reports_to":"","department":""});

        // Runs synchronously INSIDE the users transaction. Only configuration
        // and template files are read here: never re-enter users.db.
        let policy_check = || {
            let current = config::read_strict(self.home_dir());
            if !current.enabled || current.revision != req.settings_revision
                || current.avatar_template != req.template {
                return Err(POLICY_CHANGED.to_owned());
            }
            // Also stop a template replacement while staging was in progress.
            if let config::AvatarTemplate::Company { key, revision } = &req.template {
                let current = crate::company_templates::get(self.home_dir(), key)
                    .map_err(|_| TEMPLATE_INVALID.to_owned())?;
                if current.revision != *revision { return Err(TEMPLATE_CHANGED.to_owned()); }
                if current.premium || !current.members.is_empty() { return Err(TEMPLATE_INVALID.to_owned()); }
            }
            Ok(())
        };
        let avatar = AvatarCreation::SigninPolicy {
            member: &req.member_id, revision: &req.settings_revision, policy_check: &policy_check,
        };
        let response = if let Some(item) = template.as_mut() {
            let mut table: toml::Table = item.agent_toml.parse().map_err(|_| SigninAvatarError::TemplateInvalid)?;
            let agent = table.get_mut("agent").and_then(toml::Value::as_table_mut)
                .ok_or(SigninAvatarError::TemplateInvalid)?;
            for (key, value) in [("role", "specialist"), ("reports_to", ""), ("department", ""),
                ("display_name", display_name.as_str())] {
                agent.insert(key.into(), value.into());
            }
            item.agent_toml = toml::to_string(&table).map_err(|_| SigninAvatarError::TemplateInvalid)?;
            let role = crate::premium_templates::AssembledRole {
                role_id: format!("company:{}", item.id), kind: crate::premium_templates::RoleKind::Worker,
                name: item.name.clone(), display_name: display_name.clone(),
                trigger: String::new(), reports_to: String::new(), summary: item.description.clone(),
                agent_toml: item.agent_toml.clone(), soul_md: item.soul_md.clone(),
                contract_toml: item.contract_toml.clone(), extras_dir: None,
            };
            self.validate_template_policy(&role).map_err(|_| SigninAvatarError::TemplateInvalid)?;
            self.create_template_with_skills_inner(&params, None, &self.home_dir().join("company_templates"),
                &role.role_id, &role, Some(&item.skills), false, false, &item.disabled_skills, None, avatar).await
        } else {
            self.create_agent_files_inner(&params, None, avatar).await
        };
        signin_result(response, &req.agent_name)
    }
}

/// Consume existing writer responses internally. Callers receive only a fixed
/// enum, never a filesystem path, SQLite error, or arbitrary internal message.
fn signin_result(frame: WsFrame, name: &str) -> Result<(), SigninAvatarError> {
    let error = match frame {
        WsFrame::Response { ok: true, .. } => return Ok(()),
        WsFrame::Response { error: Some(error), .. } => error,
        _ => return Err(SigninAvatarError::Failed),
    };
    if error.get("code").and_then(Value::as_str) == Some(duduclaw_core::agent_seats::SEAT_LIMIT_ERROR_CODE) {
        return Err(SigninAvatarError::SeatsFull);
    }
    let text = error.as_str().or_else(|| error.get("message").and_then(Value::as_str)).unwrap_or("");
    let text = text.strip_prefix("AI 員工沒有建立：").unwrap_or(text);
    let kind = match text {
        POLICY_CHANGED => SigninAvatarError::PolicyChanged,
        TEMPLATE_CHANGED => SigninAvatarError::TemplateChanged,
        TEMPLATE_INVALID => SigninAvatarError::TemplateInvalid,
        "avatar_signin_required" | "成員必須啟用且尚未設定 AI 分身。" => SigninAvatarError::NotEligible,
        "此 AI 名稱仍有身分紀錄，請使用新的名稱。" | "Employee name already exists" => SigninAvatarError::NameTaken,
        duduclaw_core::agent_seats::AVATAR_SEAT_REFUSAL => SigninAvatarError::SeatsFull,
        _ if text == format!("Agent '{name}' already exists")
            || text == format!("agent '{name}' already exists")
            || text == format!("已有同名的 AI 員工({name}),請換一個名稱") => SigninAvatarError::NameTaken,
        _ => SigninAvatarError::Failed,
    };
    Err(kind)
}
