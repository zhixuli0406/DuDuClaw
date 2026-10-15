//! `duduclaw doctor` probe for the task sandbox: Docker reachable, sandbox
//! image present locally (never pulled), which agents enable the sandbox, and
//! which of those cannot work because `network_access = false`.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Pass,
    Warn,
}

/// Shown whenever at least one employee has the sandbox on: what the sandbox
/// does NOT cover for them (see `task_sandbox::coverage`).
pub const HOST_PATHS_NOTE: &str = "這些員工的通道回覆、排程任務與提醒仍在本機執行，不經過沙箱；\
     他們的目標任務每一輪都單獨執行，不會組成團隊；收到新郵件時也不會自動喚醒他們";

/// One sandbox-enabled agent: `(name, network_access)`.
pub type SandboxedAgent = (String, bool);

/// The verdict line. Pure, so every combination is unit-tested.
pub fn verdict(
    sandboxed: &[SandboxedAgent],
    settings: Result<&super::settings::SandboxSettings, &str>,
    docker_reachable: bool,
    image_present: bool,
) -> (Level, String) {
    let mut lines = Vec::new();
    let mut problem = false;
    let image = match settings {
        Ok(s) => Some(s.image.as_str()),
        Err(why) => {
            problem = true;
            lines.push(format!("config.toml [container.sandbox] 設定無效：{why}"));
            None
        }
    };
    if docker_reachable {
        lines.push("Docker 可連線".to_string());
    } else {
        problem = true;
        lines.push("Docker 無法連線".to_string());
    }
    if let Some(image) = image {
        if !docker_reachable {
            lines.push(format!("沙箱 image：{image}（Docker 不可用，無法確認）"));
        } else if image_present {
            lines.push(format!("沙箱 image 已在本機：{image}"));
        } else {
            problem = true;
            lines.push(format!(
                "沙箱 image 不在本機，請執行 `docker pull {image}`（不會自動下載；{}）",
                duduclaw_core::sandbox_image::OS_PULL_HINT_ZH
            ));
        }
    }
    if sandboxed.is_empty() {
        lines.push("沒有員工開啟任務沙箱（sandbox_enabled）".to_string());
        // Nothing depends on the sandbox, so its prerequisites are informational.
        return (Level::Pass, lines.join("\n         "));
    }
    let names: Vec<&str> = sandboxed.iter().map(|(n, _)| n.as_str()).collect();
    lines.push(format!("開啟沙箱的員工：{}", names.join(", ")));
    lines.push(HOST_PATHS_NOTE.to_string());
    let offline: Vec<&str> = sandboxed.iter().filter(|(_, net)| !net).map(|(n, _)| n.as_str()).collect();
    if !offline.is_empty() {
        problem = true;
        lines.push(format!(
            "這些員工 network_access = false，沙箱內的 AI 連不到模型供應商，任務一定失敗：{}",
            offline.join(", ")
        ));
    }
    if problem {
        lines.push("沙箱不能用時任務會失敗（不會改成不隔離執行）".to_string());
    }
    (if problem { Level::Warn } else { Level::Pass }, lines.join("\n         "))
}

/// Append the resource-limit self-check line (`container::resource_limits`)
/// when it failed, and always warn: an engine that ignores resource limits
/// is a broken host or image whether or not anyone uses the sandbox yet,
/// and a PASS headline above "容器功能停用" read as healthy (first rootless
/// image verification, 2026-10-15).
pub fn with_limits_line(
    (level, text): (Level, String),
    limits: Option<duduclaw_core::container_engine::LimitVerdict>,
) -> (Level, String) {
    let Some(verdict) = limits else { return (level, text) };
    let text = format!("{text}\n         {}", verdict.reason_zh());
    (Level::Warn, text)
}

/// Agents under `<home>/agents` with `[container] sandbox_enabled = true`.
pub async fn sandboxed_agents(home: &Path) -> Vec<SandboxedAgent> {
    let mut registry = duduclaw_agent::registry::AgentRegistry::new(home.join("agents"));
    if registry.scan().await.is_err() {
        return Vec::new();
    }
    let mut out: Vec<SandboxedAgent> = registry
        .list()
        .into_iter()
        .filter(|a| a.config.container.sandbox_enabled)
        .map(|a| (a.config.agent.name.clone(), a.config.container.network_access))
        .collect();
    out.sort();
    out
}

/// The whole probe: what `duduclaw doctor` prints as one check.
pub async fn check(home: &Path) -> (Level, String) {
    let (settings, _) = super::settings::load(home);
    let docker = super::container::docker_reachable().await;
    let image = match (&settings, docker) {
        (Ok(s), true) => super::container::image_present(&s.image).await,
        _ => false,
    };
    let limits = match (&settings, image) {
        (Ok(s), true) => super::container::resource_limits(&s.image).await.err(),
        _ => None,
    };
    let agents = sandboxed_agents(home).await;
    with_limits_line(verdict(&agents, settings.as_ref().map_err(String::as_str), docker, image), limits)
}
