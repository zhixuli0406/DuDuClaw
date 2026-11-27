//! Shared employee entry preflight. One read and TOML parse of the effective
//! employee artifact; all checks and the returned settings use that document.
//! Callers must handle an exempt command without entering a model pipeline.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use duduclaw_agent::registry::{AgentRegistry, LoadedAgent};
use duduclaw_core::agent_toml::AgentTomlSections;
use duduclaw_core::types::{RuntimeSection, RuntimeType, TeamConfig};

use crate::channel_reply::failure_notice::{notice_suffix, TurnFailureReason};
use crate::chat_commands::ChatCommand;
use crate::runtime_config::{self, RuntimeSettings, RuntimeSettingsError};

/// Command identities, not spellings: aliases share the parser's exact result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExemptCommand {
    SafetyStop, SafetyStopAll, SafetyResume, SafetyStatus,
    Takeover, Status, Help, New, Pair, Model,
}

/// D1 option B. Exemption never waives access checks or command authorization.
pub const EXEMPT_COMMANDS: &[ExemptCommand] = &[
    ExemptCommand::SafetyStop,    // Halt the current scope even with broken settings.
    ExemptCommand::SafetyStopAll, // Emergency halt; the command still requires admin.
    ExemptCommand::SafetyResume,  // Recover a halted scope without calling a model.
    ExemptCommand::SafetyStatus,  // Inspect failsafe state without calling a model.
    ExemptCommand::Takeover,      // Human control must remain available during failure.
    ExemptCommand::Status,        // Read operational status without calling a model.
    ExemptCommand::Help,          // Explain available controls without calling a model.
    ExemptCommand::New,           // Reset history; existing shared-reset checks still apply.
    ExemptCommand::Pair,          // Restore access through the existing pairing flow.
    ExemptCommand::Model,         // Inspect/repair selection; setter remains admin-only.
];

pub fn command_is_exempt(command: &ChatCommand) -> bool {
    let kind = match command {
        ChatCommand::SafetyStop => ExemptCommand::SafetyStop,
        ChatCommand::SafetyStopAll => ExemptCommand::SafetyStopAll,
        ChatCommand::SafetyResume => ExemptCommand::SafetyResume,
        ChatCommand::SafetyStatus => ExemptCommand::SafetyStatus,
        ChatCommand::Status => ExemptCommand::Status,
        ChatCommand::Help => ExemptCommand::Help,
        ChatCommand::New => ExemptCommand::New,
        ChatCommand::Pair(_) => ExemptCommand::Pair,
        ChatCommand::Model(_) => ExemptCommand::Model,
        _ => return false,
    };
    EXEMPT_COMMANDS.contains(&kind)
}

/// Takeover has its own parser and authorization path, outside ChatCommand.
pub fn takeover_is_exempt(_command: &crate::chat_commands::TakeoverCommand) -> bool {
    EXEMPT_COMMANDS.contains(&ExemptCommand::Takeover)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryRefusal {
    AgentNotFound,
    Config(RuntimeSettingsError),
    InactiveRuntime { diagnostic: String },
    ModelProviderMismatch { model: String, provider: RuntimeType },
    ProductPolicy { diagnostic: String },
    SystemPromptUnreadable,
}

impl EntryRefusal {
    /// Stable transport codes. Config details remain typed diagnostics, keeping
    /// the v1.84 operator code unchanged for all strict-read failures.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AgentNotFound => "agent_not_found",
            Self::Config(_) | Self::SystemPromptUnreadable => "agent_config_unreadable",
            Self::InactiveRuntime { .. } => "runtime_unavailable",
            Self::ModelProviderMismatch { .. } => "model_provider_mismatch",
            Self::ProductPolicy { .. } => "runtime_policy_refused",
        }
    }

    /// The only entry-refusal formatter. Never expose diagnostics, paths or
    /// codes to public visitors, including authenticated widget principals.
    pub fn user_text(&self, session: &str, language: Option<&str>, turn: &str) -> String {
        if crate::channel_reply::assembly::public_session(session) {
            return crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE.into();
        }
        let reason = match self {
            Self::ModelProviderMismatch { .. } => TurnFailureReason::ModelProviderMismatch,
            Self::AgentNotFound | Self::InactiveRuntime { .. } => TurnFailureReason::Unavailable,
            Self::ProductPolicy { .. } => TurnFailureReason::SafetyGate,
            Self::Config(_) | Self::SystemPromptUnreadable => TurnFailureReason::AgentConfigUnreadable,
        };
        format!("{}\n{}", reason.text(language), notice_suffix(language, Some(self.code()), turn))
    }
}

/// Anchored operator-only marker shared by background producers and failover.
pub(crate) const ENTRY_REFUSAL_PREFIX: &str = "ENTRY_REFUSED:";

const REFUSAL_CAPACITY: usize = 1024;
const REFUSAL_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct BackgroundRefusals {
    issued: HashMap<String, (&'static str, Instant)>,
}

impl BackgroundRefusals {
    fn mint(&mut self, refusal: &EntryRefusal, now: Instant) -> String {
        self.issued.retain(|_, (_, issued)| now.duration_since(*issued) < REFUSAL_TTL);
        if self.issued.len() >= REFUSAL_CAPACITY {
            if let Some(oldest) = self.issued.iter().min_by_key(|(_, (_, issued))| *issued)
                .map(|(id, _)| id.clone()) {
                self.issued.remove(&oldest);
            }
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let code = refusal.code();
        self.issued.insert(id.clone(), (code, now));
        format!("{ENTRY_REFUSAL_PREFIX}{code}:{id}: employee configuration refused")
    }

    /// One parser for all consumers. Provider text cannot mint an admission refusal.
    fn code<'a>(&self, error: &'a str, now: Instant) -> Option<&'a str> {
        let (code, rest) = error.strip_prefix(ENTRY_REFUSAL_PREFIX)?.split_once(':')?;
        match code {
            "agent_not_found" | "agent_config_unreadable" | "runtime_unavailable"
            | "model_provider_mismatch" | "runtime_policy_refused" => {}
            _ => return None,
        }
        let (id, _) = rest.split_once(':')?;
        let (registered_code, issued) = self.issued.get(id)?;
        (*registered_code == code && now.duration_since(*issued) < REFUSAL_TTL).then_some(code)
    }
}

fn background_refusals() -> &'static Mutex<BackgroundRefusals> {
    static REFUSALS: OnceLock<Mutex<BackgroundRefusals>> = OnceLock::new();
    REFUSALS.get_or_init(|| Mutex::new(BackgroundRefusals::default()))
}

pub(crate) fn background_refusal(refusal: &EntryRefusal) -> String {
    background_refusals().lock().unwrap_or_else(|e| e.into_inner())
        .mint(refusal, Instant::now())
}

pub(crate) fn background_refusal_with_fallback(
    refusal: &EntryRefusal, fallback: Option<RuntimeType>,
) -> String {
    let error = background_refusal(refusal);
    if matches!(refusal, EntryRefusal::ModelProviderMismatch { .. }) {
        runtime_config::alignment_refusal_with_fallback(error, fallback)
    } else { error }
}

/// Never infer admission from provider prose or substring matches.
pub(crate) fn background_refusal_code(error: &str) -> Option<&str> {
    background_refusals().lock().unwrap_or_else(|e| e.into_inner())
        .code(error, Instant::now())
}

/// Owned so a caller can release the registry lock before running commands or
/// spawning. `registry_agent` retains cached prompt assets/metadata explicitly;
/// `document` and `sections` are the fresh effective configuration for this turn.
#[derive(Debug, Clone)]
pub struct ResolvedTurnConfig {
    pub registry_agent: LoadedAgent,
    pub config_path: PathBuf,
    /// Whether config_path is the preset-resolved artifact (never raw fallback
    /// after a resolved artifact was found but could not be read).
    pub preset_resolved: bool,
    pub runtime_settings: RuntimeSettings,
    /// Runnable model, after the existing bare-Claude-alias substitution.
    pub model: String,
    pub local_model_id: Option<String>,
    pub sections: AgentTomlSections,
    /// Preserves fields outside AgentTomlSections (e.g. model.effort/local) so
    /// W2 can replace downstream readers without another disk read or parse.
    pub document: toml::Value,
}

pub fn resolve_and_check(
    home: &Path, registry: &AgentRegistry, agent_id: &str,
) -> Result<ResolvedTurnConfig, EntryRefusal> {
    resolve_with_reader(home, registry, agent_id, |path| std::fs::read_to_string(path))
}

fn resolve_with_reader(
    home: &Path, registry: &AgentRegistry, agent_id: &str,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
) -> Result<ResolvedTurnConfig, EntryRefusal> {
    let agent = registry.get(agent_id).ok_or(EntryRefusal::AgentNotFound)?;
    resolve_loaded_with_reader(home, agent, agent_id, read)
}

/// Ephemeral/team members have an authoritative scaffold outside the registry.
/// Check that artifact once, just as for a registered employee.
pub(crate) fn resolve_loaded_and_check(
    home: &Path, agent: &LoadedAgent, agent_id: &str,
) -> Result<ResolvedTurnConfig, EntryRefusal> {
    resolve_loaded_with_reader(home, agent, agent_id, |path| std::fs::read_to_string(path))
}

fn resolve_loaded_with_reader(
    home: &Path, agent: &LoadedAgent, agent_id: &str,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
) -> Result<ResolvedTurnConfig, EntryRefusal> {
    resolve_loaded_with_settings(home, agent, agent_id, read, |_| {})
}

/// Preserve fallback diagnostics from the same strict read, even when model
/// alignment refuses admission before a complete turn snapshot can be returned.
pub(crate) fn resolve_loaded_for_background(
    home: &Path, agent: &LoadedAgent, agent_id: &str,
) -> Result<ResolvedTurnConfig, (EntryRefusal, Option<RuntimeType>)> {
    let mut fallback = None;
    resolve_loaded_with_settings(home, agent, agent_id,
        |path| std::fs::read_to_string(path), |settings| fallback = settings.fallback)
        .map_err(|refusal| (refusal, fallback))
}

fn resolve_loaded_with_settings(
    home: &Path, agent: &LoadedAgent, agent_id: &str,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
    on_settings: impl FnOnce(&RuntimeSettings),
) -> Result<ResolvedTurnConfig, EntryRefusal> {
    let artifact = check_artifact_with_settings(home, &agent.dir, agent_id, read, on_settings)?;
    Ok(ResolvedTurnConfig {
        registry_agent: agent.clone(), config_path: artifact.path,
        preset_resolved: artifact.preset_resolved,
        runtime_settings: artifact.settings, model: artifact.model,
        local_model_id: artifact.local_model_id, sections: artifact.sections,
        document: artifact.document,
    })
}

/// Task producers also run in the MCP process, without a live registry.
/// Check the effective artifact directly, without scanning the registry (a
/// scan can rewrite preset artifacts). This uses the exact same strict gate.
pub fn check_task_assignee(home: &Path, agent_id: &str) -> Result<(), EntryRefusal> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err(EntryRefusal::AgentNotFound);
    }
    let seat = duduclaw_core::agent_seats::resolve_agent_seat(home, agent_id)
        .map_err(|_| EntryRefusal::AgentNotFound)?
        .ok_or(EntryRefusal::AgentNotFound)?;
    check_artifact(home, &home.join("agents").join(seat.id), agent_id,
        |path| std::fs::read_to_string(path)).map(|_| ())
}

/// Classify an unavailable assignee's malformed profile without allowing it.
/// Call only after authorization and a failed roster check. Missing/deleted/
/// archived employees retain the roster error; malformed files get safe text.
pub fn malformed_assignee_refusal(home: &Path, agent_id: &str) -> Option<EntryRefusal> {
    let seat = duduclaw_core::agent_seats::resolve_agent_seat(home, agent_id).ok()??;
    let text = std::fs::read_to_string(home.join("agents").join(seat.id).join("agent.toml")).ok()?;
    text.parse::<toml::Table>().err()
        .map(|_| EntryRefusal::Config(RuntimeSettingsError::Malformed))
}

/// Check the durable assignee, never a card's display label or chat selection.
/// The caller must already have authorized access to this task.
pub(crate) async fn check_task_start(
    home: &Path, task_id: &str, session: &str, turn: &str,
) -> Result<(), String> {
    let unavailable = || {
        if crate::channel_reply::assembly::public_session(session) {
            crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE.to_string()
        } else {
            "目前無法開始這項工作，請稍後再試。".to_string()
        }
    };
    let store = crate::task_store::TaskStore::open(home).map_err(|_| unavailable())?;
    let task = store.get_task(task_id).await.map_err(|_| unavailable())?.ok_or_else(unavailable)?;
    if let Err(refusal) = check_task_assignee(home, &task.assigned_to) {
        let language = crate::prompt_identity::resolve_fixed_message_language_for_id(home, &task.assigned_to).await;
        return Err(refusal.user_text(session, language.as_deref(), turn));
    }
    Ok(())
}

/// Denial never checks configuration. Approval is checked only when it releases
/// employee work; management/configuration repair cards retain their behavior.
pub(crate) async fn check_approval_start(
    home: &Path, record: &crate::approval::ApprovalRecord, approve: bool, session: &str,
) -> Result<(), String> {
    if !approve { return Ok(()); }
    let task_id = record.binding.as_ref().and_then(|binding| binding.task_id.clone())
        .or_else(|| crate::handlers::approval_task_ref(&record.payload));
    if let Some(task_id) = task_id {
        return check_task_start(home, &task_id, session, record.id.as_str()).await;
    }
    // A kickoff without its durable task is never allowed to authorize work.
    if record.action_kind == "goal_kickoff" {
        return Err(EntryRefusal::AgentNotFound.user_text(session, None, record.id.as_str()));
    }
    Ok(())
}

struct CheckedArtifact {
    path: PathBuf,
    preset_resolved: bool,
    settings: RuntimeSettings,
    model: String,
    local_model_id: Option<String>,
    sections: AgentTomlSections,
    document: toml::Value,
}

fn check_artifact(
    home: &Path, agent_dir: &Path, agent_id: &str,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
) -> Result<CheckedArtifact, EntryRefusal> {
    check_artifact_with_settings(home, agent_dir, agent_id, read, |_| {})
}

fn check_artifact_with_settings(
    home: &Path, agent_dir: &Path, agent_id: &str,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
    on_settings: impl FnOnce(&RuntimeSettings),
) -> Result<CheckedArtifact, EntryRefusal> {
    // Use the same effective artifact selection as load_runtime_settings_strict.
    let mut path = agent_dir.join("agent.toml");
    let mut preset_resolved = false;
    if let Some(agent_home) = duduclaw_core::preset::agent_home_dir(agent_dir) {
        if let Some(id) = agent_dir.file_name().and_then(|id| id.to_str()) {
            let resolved = duduclaw_core::preset::agent_resolved_path(&agent_home, id);
            match std::fs::symlink_metadata(&resolved) {
                Ok(_) => { path = resolved; preset_resolved = true; }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(EntryRefusal::Config(RuntimeSettingsError::Unreadable)),
            }
        }
    }
    // FnOnce makes a second employee read impossible. There is exactly one
    // text-to-TOML parse; typed projections below deserialize this same tree.
    let text = read(&path).map_err(|error| EntryRefusal::Config(
        if error.kind() == std::io::ErrorKind::NotFound { RuntimeSettingsError::Missing }
        else { RuntimeSettingsError::Unreadable }
    ))?;
    let document: toml::Value = text.parse().map_err(|_| EntryRefusal::Config(RuntimeSettingsError::Malformed))?;
    let (sections, mut settings, local_model_id) = strict_settings(&document, &path)?;
    on_settings(&settings);
    let global_team = crate::team_composer::read_global_team_config(home)
        .map_err(|_| EntryRefusal::Config(RuntimeSettingsError::Malformed))?;
    runtime_config::ensure_active_team_providers(&global_team, &sections.team)
        .map_err(|diagnostic| EntryRefusal::InactiveRuntime { diagnostic })?;
    // The legacy gate checks removed global roles; reject unknown selectors
    // in the merged team too, instead of letting a later reader default them.
    let team = TeamConfig::merge(&global_team, &sections.team);
    for role in duduclaw_core::types::Role::ALL {
        if let Some(runtime) = team.roles.get(*role).runtime.as_deref() {
            strict_provider(runtime)?;
        }
    }
    let configured = settings.preferred_or_default().map_err(EntryRefusal::Config)?;
    let model = check_model(agent_id, &configured, settings.provider)?;
    crate::product_policy::require_runtime(home, settings.provider.as_str())
        .map_err(|diagnostic| EntryRefusal::ProductPolicy { diagnostic })?;
    if model != configured {
        crate::product_policy::load(home).and_then(|policy|
            crate::product_policy::require_substituted_catalog_model(policy, settings.provider, &model)
        ).map_err(|diagnostic| EntryRefusal::ProductPolicy { diagnostic })?;
    }
    settings.preferred_model = Some(model.clone());
    Ok(CheckedArtifact { path, preset_resolved, settings, model, local_model_id, sections, document })
}

/// Also used by the legacy second gate until W2 removes that re-read.
pub(crate) fn check_model(agent: &str, model: &str, provider: RuntimeType) -> Result<String, EntryRefusal> {
    let model = runtime_config::runnable_model(agent, model, provider).into_owned();
    runtime_config::ensure_model_provider(&model, provider)
        .map_err(|_| EntryRefusal::ModelProviderMismatch { model: model.clone(), provider })?;
    Ok(model)
}

fn strict_provider(value: &str) -> Result<RuntimeType, EntryRefusal> {
    if let Some(removed) = duduclaw_core::runtime_catalog::removed_runtime(value) {
        return Err(EntryRefusal::InactiveRuntime { diagnostic: removed.refusal("employee runtime") });
    }
    RuntimeType::parse(value).ok_or(EntryRefusal::Config(RuntimeSettingsError::UnknownProvider))
}

fn strict_settings(
    doc: &toml::Value, path: &Path,
) -> Result<(AgentTomlSections, RuntimeSettings, Option<String>), EntryRefusal> {
    let malformed = || EntryRefusal::Config(RuntimeSettingsError::Malformed);
    runtime_config::check_runtime_field_types(doc, path).map_err(|_| malformed())?;
    runtime_config::check_removed_agent_runtimes(doc, path)
        .map_err(|diagnostic| EntryRefusal::InactiveRuntime { diagnostic })?;
    // Deserialize runtime directly, not through the projection's or_default.
    let runtime = doc.get("runtime").map(|value| value.clone().try_into::<RuntimeSection>())
        .transpose().map_err(|_| malformed())?.unwrap_or_default();
    let provider = runtime.provider.as_deref().map(strict_provider).transpose()?
        .unwrap_or(RuntimeType::Claude); // Absent selector retains the schema default, never an error fallback.
    let fallback = runtime.fallback.as_deref().map(strict_provider).transpose()?;
    let model = doc.get("model").map(|value| value.as_table().ok_or_else(malformed))
        .transpose()?;
    let preferred = model.and_then(|model| model.get("preferred"))
        .map(|value| value.as_str().map(str::to_string).ok_or_else(malformed)).transpose()?;
    let utility = model.and_then(|model| model.get("utility"))
        .map(|value| value.as_str().ok_or_else(malformed)).transpose()?
        .filter(|value| !value.trim().is_empty()).map(str::to_string);
    let local_model_id = model.and_then(|model| model.get("local"))
        .map(|value| value.as_table().ok_or_else(malformed))
        .transpose()?.and_then(|local| local.get("model"))
        .map(|value| value.as_str().map(str::to_string).ok_or_else(malformed)).transpose()?;
    let team = runtime_config::strict_team_config(doc, path).map_err(|_| malformed())?;
    for role in duduclaw_core::types::Role::ALL {
        if let Some(value) = team.roles.get(*role).runtime.as_deref() { strict_provider(value)?; }
    }
    let mut sections = doc.clone().try_into::<AgentTomlSections>().map_err(|_| malformed())?;
    sections.runtime = runtime;
    sections.team = team;
    let mut settings = RuntimeSettings {
        provider, fallback, preferred_model: preferred, utility_model: String::new(),
    };
    // Retain the strict reader's catalog defaults and Kiro's CLI-selected model.
    let preferred = settings.preferred_or_default().map_err(EntryRefusal::Config)?;
    settings.utility_model = utility.unwrap_or_else(|| if provider == RuntimeType::Claude {
        runtime_config::DEFAULT_UTILITY_MODEL.to_string()
    } else { preferred.clone() });
    settings.preferred_model = Some(preferred);
    Ok((sections, settings, local_model_id))
}

#[cfg(test)]
#[path = "entry_gate_tests.rs"]
mod tests;
