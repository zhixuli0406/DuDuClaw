//! Which container image an L5a computer-use session runs, and whether it is
//! present locally.
//!
//! Resolution, highest first:
//! 1. `config.toml [computer_use] image = "<ref>"` — one global operator key,
//!    validated with [`duduclaw_core::sandbox_image::valid_image`] plus the
//!    orchestrator's own argv-safety check. An invalid value (or an invalid
//!    `[computer_use]` table) makes computer use unavailable with a clear
//!    message; it never falls back to the default.
//! 2. [`default_image`]: `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway
//!    version>`, published by `.github/workflows/computer-use-image.yml`.
//!
//! There is no per-agent key: `agent.toml [capabilities.computer_use_config]`
//! has no image field, and the orchestrator's `ComputerUseConfig` is built in
//! code, so the global key is the only override.
//!
//! The image is never pulled: `docker run` carries `--pull never`, and a
//! session start first checks `docker image inspect`. A missing image fails
//! the start with [`missing_message`], which names the image and the remedy.

use std::path::Path;
use std::time::Duration;

use tracing::warn;

use crate::task_sandbox::doctor::Level;

/// The tag `docker build` gives a locally built image (see
/// [`LOCAL_BUILD_COMMAND`]); set it as the override to use such a build.
pub const LOCAL_BUILD_IMAGE: &str = "duduclaw-computer-use:latest";

/// How to build the image from a source checkout (run at the repo root).
pub const LOCAL_BUILD_COMMAND: &str =
    "docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .";

/// Upper bound on one `docker image inspect`.
pub(crate) const IMAGE_INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Keys `[computer_use]` may carry. Anything else is a typo and makes the
/// table invalid (fail closed) instead of silently using the default.
const KNOWN_KEYS: &[&str] = &["image"];

/// `ghcr.io/zhixuli0406/duduclaw-computer-use:v<this gateway's version>`.
pub fn default_image() -> String {
    duduclaw_core::sandbox_image::computer_use_image(env!("CARGO_PKG_VERSION"))
}

/// An override value acceptable both as an image reference and as the last
/// `docker run` argument.
fn acceptable(image: &str) -> bool {
    duduclaw_core::sandbox_image::valid_image(image)
        && crate::computer_use_orchestrator::image_name_is_safe(image)
}

/// The image from a parsed `config.toml`. A missing `[computer_use]` table or
/// a missing `image` key is the default; anything malformed is an error.
pub fn parse(config: &toml::Table) -> Result<String, String> {
    let table = match config.get("computer_use") {
        None => return Ok(default_image()),
        Some(toml::Value::Table(table)) => table,
        Some(_) => return Err("[computer_use] must be a table".into()),
    };
    if let Some(unknown) = table.keys().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
        return Err(format!("[computer_use] has an unknown key `{unknown}`"));
    }
    match table.get("image") {
        None => Ok(default_image()),
        Some(toml::Value::String(image)) if acceptable(image.trim()) => Ok(image.trim().to_string()),
        Some(_) => Err("[computer_use] image is not a valid image reference".into()),
    }
}

/// The image from `<home>/config.toml`. A missing file is the default; an
/// unreadable or unparsable one is an error (never the default).
pub fn load(home: &Path) -> Result<String, String> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => parse(&table),
            Err(_) => Err("config.toml is not valid TOML".into()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_image()),
        Err(_) => Err("config.toml could not be read".into()),
    }
}

/// Session-start text when the image is not present locally. Operator-safe:
/// names the image and the remedy, carries no Docker output.
pub fn missing_message(image: &str) -> String {
    format!(
        "電腦操作無法啟動：本機沒有 image {image}，系統不會自動下載。\
         請在主機執行 `docker pull {image}`；\
         或在 DuDuClaw 原始碼根目錄執行 `{LOCAL_BUILD_COMMAND}` 自行建置，\
         再到 config.toml 設定 [computer_use] image = \"{LOCAL_BUILD_IMAGE}\"。"
    )
}

/// Session-start text when Docker is unavailable (shared probe) or did not
/// answer the presence check.
pub fn unchecked_message(image: &str) -> String {
    format!(
        "電腦操作無法啟動：無法確認 image {image} 是否在本機（Docker 沒有回應）。\
         請確認 Docker 正在執行後再試。"
    )
}

/// Session-start text when `[computer_use]` in `config.toml` is invalid.
pub fn invalid_config_message(why: &str) -> String {
    format!("電腦操作無法啟動：config.toml 的 [computer_use] 設定無效（{why}），請修正後再試。")
}

/// The doctor's line naming employees still set to the removed native mode.
/// The `computer_*` tools refuse them; nothing falls back to a container.
pub fn native_mode_line(native_users: &[String]) -> String {
    format!(
        "以下員工仍設定 [capabilities] computer_use_mode = \"native\"：{}。\
         直接操作主機桌面的模式已移除，computer_* 工具會拒絕這些員工；\
         請刪除這個鍵或改為 \"container\"",
        native_users.join(", ")
    )
}

/// Result of `docker image inspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The image is in the local store.
    Present,
    /// Docker answered and the image is not there (a non-zero exit).
    Missing,
    /// Docker could not be run or did not answer in time.
    Unknown,
}

/// `docker image inspect <image>`, bounded by [`IMAGE_INSPECT_TIMEOUT`],
/// after the shared Docker probe (`task_sandbox::container::docker_status`)
/// says the daemon is usable; otherwise [`Presence::Unknown`]. Never pulls. An invalid reference is reported as missing without running
/// Docker at all.
pub async fn image_presence(image: &str) -> Presence {
    if !acceptable(image) {
        return Presence::Missing;
    }
    // A half-dead daemon fails `image inspect` with a non-zero exit, which
    // would read as "image missing" and send the operator to `docker pull`.
    // Ask the shared probe first so session start, doctor and the task
    // sandbox agree on whether Docker is usable at all.
    if let duduclaw_core::docker_probe::DockerStatus::Unavailable(why) =
        crate::task_sandbox::container::docker_status().await
    {
        warn!(reason = why.code(), "computer-use image check skipped: Docker unavailable");
        return Presence::Unknown;
    }
    let args = ["image", "inspect", "--format", "{{.Id}}", image];
    match crate::computer_use_orchestrator::docker_output(&args, IMAGE_INSPECT_TIMEOUT, "Image inspect")
        .await
    {
        Ok(out) if out.status.success() && !out.stdout.trim_ascii().is_empty() => Presence::Present,
        Ok(_) => Presence::Missing,
        Err(e) => {
            warn!(error = %e, "computer-use image presence check did not complete");
            Presence::Unknown
        }
    }
}

/// The text a failed presence check turns into, or `None` when present.
pub fn presence_error(image: &str, presence: Presence) -> Option<String> {
    match presence {
        Presence::Present => None,
        Presence::Missing => Some(missing_message(image)),
        Presence::Unknown => Some(unchecked_message(image)),
    }
}

/// Resolve the image from `<home>/config.toml` and check it is present.
/// `Err` carries the operator-safe message for the caller.
pub async fn preflight(home: &Path) -> Result<String, String> {
    let image = load(home).map_err(|why| invalid_config_message(&why))?;
    match presence_error(&image, image_presence(&image).await) {
        None => Ok(image),
        Some(message) => Err(message),
    }
}

// ---------------------------------------------------------------------------
// duduclaw doctor
// ---------------------------------------------------------------------------

/// The doctor verdict. Pure, so every combination is unit-tested.
///
/// `users` are the employees with `[capabilities] computer_use = true`;
/// `native_users` are the employees still set to `computer_use_mode =
/// "native"` (removed; always a warning, whether or not computer use is on).
pub fn verdict(
    users: &[String],
    image: Result<&str, &str>,
    docker_reachable: bool,
    image_present: bool,
    native_users: &[String],
) -> (Level, String) {
    let mut lines = Vec::new();
    let mut problem = false;
    match image {
        Ok(image) => {
            lines.push(format!("電腦操作 image：{image}"));
            if !docker_reachable {
                problem = true;
                lines.push("Docker 無法連線，無法確認 image 是否在本機".to_string());
            } else if image_present {
                lines.push("image 已在本機".to_string());
            } else {
                problem = true;
                lines.push(format!(
                    "image 不在本機（不會自動下載），請執行 `docker pull {image}`；\
                     或在原始碼根目錄執行 `{LOCAL_BUILD_COMMAND}`，\
                     再設定 config.toml [computer_use] image = \"{LOCAL_BUILD_IMAGE}\""
                ));
            }
        }
        Err(why) => {
            problem = true;
            lines.push(format!("config.toml [computer_use] 設定無效：{why}"));
        }
    }
    let native = !native_users.is_empty();
    if users.is_empty() {
        lines.push("沒有員工開啟電腦操作（[capabilities] computer_use）".to_string());
        if native {
            lines.push(native_mode_line(native_users));
        }
        // Nothing depends on the image, so its state is informational; a
        // leftover native setting still warns.
        let level = if native { Level::Warn } else { Level::Pass };
        return (level, lines.join("\n         "));
    }
    lines.push(format!("開啟電腦操作的員工：{}", users.join(", ")));
    if native {
        lines.push(native_mode_line(native_users));
    }
    if problem {
        lines.push("在上述問題解決前，這些員工呼叫 computer_* 工具會直接失敗並說明原因".to_string());
    }
    (if problem || native { Level::Warn } else { Level::Pass }, lines.join("\n         "))
}

/// Employees under `<home>/agents` with `[capabilities] computer_use = true`.
pub async fn computer_use_agents(home: &Path) -> Vec<String> {
    let mut registry = duduclaw_agent::registry::AgentRegistry::new(home.join("agents"));
    if registry.scan().await.is_err() {
        return Vec::new();
    }
    let mut out: Vec<String> = registry
        .list()
        .into_iter()
        .filter(|a| a.config.capabilities.computer_use)
        .map(|a| a.config.agent.name.clone())
        .collect();
    out.sort();
    out
}

/// Employees under `<home>/agents` still set to `[capabilities]
/// computer_use_mode = "native"`, sorted.
pub async fn native_mode_agents(home: &Path) -> Vec<String> {
    let mut registry = duduclaw_agent::registry::AgentRegistry::new(home.join("agents"));
    if registry.scan().await.is_err() {
        return Vec::new();
    }
    let mut out: Vec<String> = registry
        .list()
        .into_iter()
        .filter(|a| a.config.capabilities.computer_use_mode == duduclaw_core::types::ComputerUseMode::Native)
        .map(|a| a.config.agent.name.clone())
        .collect();
    out.sort();
    out
}

/// Per employee with computer use on: `(name, usable allowlist hosts,
/// ignored entries)` from `[capabilities.computer_use_config]
/// allowed_domains`, sorted by name.
pub async fn computer_use_allowlists(home: &Path) -> Vec<(String, usize, usize)> {
    let mut registry = duduclaw_agent::registry::AgentRegistry::new(home.join("agents"));
    if registry.scan().await.is_err() {
        return Vec::new();
    }
    let mut out: Vec<(String, usize, usize)> = registry
        .list()
        .into_iter()
        .filter(|a| a.config.capabilities.computer_use)
        .map(|a| {
            let nav = a.config.capabilities.computer_use_config.navigation_hosts();
            (a.config.agent.name.clone(), nav.hosts.len(), nav.dropped.len())
        })
        .collect();
    out.sort();
    out
}

/// The doctor line about navigation allowlists (empty input ⇒ `None`).
pub fn allowlist_line(rows: &[(String, usize, usize)]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let parts: Vec<String> = rows
        .iter()
        .map(|(name, hosts, ignored)| {
            let mut part = if *hosts == 0 {
                format!("{name} 沒有可用的網域，電腦操作時沒有網路")
            } else {
                format!("{name} 可開啟 {hosts} 個網域")
            };
            if *ignored > 0 {
                part.push_str(&format!("（另有 {ignored} 個項目格式不符或超過上限，已略過）"));
            }
            part
        })
        .collect();
    Some(format!(
        "網站白名單（[capabilities.computer_use_config] allowed_domains）：{}",
        parts.join("；")
    ))
}

/// The whole probe: what `duduclaw doctor` prints as one row.
pub async fn check(home: &Path) -> (Level, String) {
    let image = load(home);
    let docker = crate::task_sandbox::container::docker_reachable().await;
    let present = match (&image, docker) {
        (Ok(image), true) => image_presence(image).await == Presence::Present,
        _ => false,
    };
    let users = computer_use_agents(home).await;
    let native_users = native_mode_agents(home).await;
    let (level, mut text) = verdict(
        &users,
        image.as_ref().map(String::as_str).map_err(String::as_str),
        docker,
        present,
        &native_users,
    );
    if let Some(line) = allowlist_line(&computer_use_allowlists(home).await) {
        text.push_str("\n         ");
        text.push_str(&line);
    }
    (level, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_line_counts_hosts_per_employee() {
        assert_eq!(allowlist_line(&[]), None);
        let line = allowlist_line(&[("alice".into(), 3, 0), ("bob".into(), 0, 2)]).unwrap();
        assert!(line.contains("alice 可開啟 3 個網域"), "{line}");
        assert!(line.contains("bob 沒有可用的網域，電腦操作時沒有網路（另有 2 個項目"), "{line}");
        assert!(line.contains("allowed_domains"), "{line}");
    }

    fn table(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    #[test]
    fn default_is_the_versioned_published_image() {
        let image = default_image();
        assert_eq!(
            image,
            format!("ghcr.io/zhixuli0406/duduclaw-computer-use:v{}", env!("CARGO_PKG_VERSION"))
        );
        assert!(!image.ends_with(":latest"));
    }

    #[test]
    fn resolution_precedence_global_key_then_default() {
        assert_eq!(parse(&table("")).unwrap(), default_image());
        assert_eq!(parse(&table("[computer_use]\n")).unwrap(), default_image());
        assert_eq!(
            parse(&table("[computer_use]\nimage = \"duduclaw-computer-use:latest\"\n")).unwrap(),
            "duduclaw-computer-use:latest"
        );
        assert_eq!(
            parse(&table("[computer_use]\nimage = \"  registry.example/cu:v2  \"\n")).unwrap(),
            "registry.example/cu:v2"
        );
        assert_eq!(
            parse(&table(
                "[computer_use]\nimage = \"ghcr.io/x/cu@sha256:be45dfabcca9ecb7c783fd06b08a98bc33ab37255684cf14bed46327f8d98776\"\n"
            ))
            .unwrap(),
            "ghcr.io/x/cu@sha256:be45dfabcca9ecb7c783fd06b08a98bc33ab37255684cf14bed46327f8d98776"
        );
    }

    #[test]
    fn invalid_override_is_an_error_never_the_default() {
        for bad in [
            "[computer_use]\nimage = \"\"\n",
            "[computer_use]\nimage = \"--privileged\"\n",
            "[computer_use]\nimage = \"a b\"\n",
            "[computer_use]\nimage = \"img;rm\"\n",
            "[computer_use]\nimage = 3\n",
            "[computer_use]\nimgae = \"x:y\"\n",
            "computer_use = \"x:y\"\n",
        ] {
            assert!(parse(&table(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn load_reads_home_config_and_fails_closed_on_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()).unwrap(), default_image());
        std::fs::write(tmp.path().join("config.toml"), "[computer_use]\nimage = \"local/cu:dev\"\n")
            .unwrap();
        assert_eq!(load(tmp.path()).unwrap(), "local/cu:dev");
        std::fs::write(tmp.path().join("config.toml"), "[computer_use\n").unwrap();
        assert!(load(tmp.path()).is_err());
    }

    #[test]
    fn missing_image_text_names_the_image_and_both_remedies() {
        let m = missing_message("ghcr.io/zhixuli0406/duduclaw-computer-use:v1.67.0");
        assert!(m.contains("docker pull ghcr.io/zhixuli0406/duduclaw-computer-use:v1.67.0"), "{m}");
        assert!(m.contains(LOCAL_BUILD_COMMAND), "{m}");
        assert!(m.contains("[computer_use] image = \"duduclaw-computer-use:latest\""), "{m}");
        assert_eq!(presence_error("x:y", Presence::Missing), Some(missing_message("x:y")));
        assert_eq!(presence_error("x:y", Presence::Unknown), Some(unchecked_message("x:y")));
        assert_eq!(presence_error("x:y", Presence::Present), None);
    }

    #[tokio::test]
    async fn invalid_reference_is_missing_without_running_docker() {
        assert_eq!(image_presence("--help").await, Presence::Missing);
    }

    #[test]
    fn doctor_passes_when_no_employee_uses_computer_use() {
        let (level, text) = verdict(&[], Ok("x:y"), true, false, &[]);
        assert_eq!(level, Level::Pass, "{text}");
        assert!(text.contains("沒有員工開啟電腦操作"), "{text}");
        let (level, _) = verdict(&[], Err("bad"), false, false, &[]);
        assert_eq!(level, Level::Pass);
    }

    #[test]
    fn doctor_warns_with_docker_pull_when_users_exist_and_image_is_missing() {
        let users = vec!["alice".to_string()];
        let (level, text) = verdict(&users, Ok("ghcr.io/x/cu:v1"), true, false, &[]);
        assert_eq!(level, Level::Warn, "{text}");
        assert!(text.contains("docker pull ghcr.io/x/cu:v1"), "{text}");
        assert!(text.contains("alice"), "{text}");
    }

    #[test]
    fn doctor_passes_when_users_exist_and_image_is_present() {
        let users = vec!["alice".to_string(), "bob".to_string()];
        let (level, text) = verdict(&users, Ok("ghcr.io/x/cu:v1"), true, true, &[]);
        assert_eq!(level, Level::Pass, "{text}");
        assert!(text.contains("image 已在本機") && text.contains("alice, bob"), "{text}");
    }

    #[test]
    fn doctor_warns_on_invalid_config_or_unreachable_docker_when_users_exist() {
        let users = vec!["alice".to_string()];
        let (level, text) = verdict(&users, Err("[computer_use] image is not a valid image reference"), true, false, &[]);
        assert_eq!(level, Level::Warn, "{text}");
        assert!(text.contains("設定無效"), "{text}");
        let (level, text) = verdict(&users, Ok("x:y"), false, false, &[]);
        assert_eq!(level, Level::Warn, "{text}");
        assert!(text.contains("Docker 無法連線"), "{text}");
    }

    #[test]
    fn doctor_warns_and_names_employees_still_set_to_native() {
        let native = vec!["desk-bot".to_string()];
        // With computer-use employees and a present image: still a warning.
        let users = vec!["alice".to_string(), "desk-bot".to_string()];
        let (level, text) = verdict(&users, Ok("ghcr.io/x/cu:v1"), true, true, &native);
        assert_eq!(level, Level::Warn, "{text}");
        assert!(text.contains(&native_mode_line(&native)), "{text}");
        assert!(text.contains("desk-bot") && text.contains("已移除"), "{text}");
        assert!(text.contains("\"container\""), "{text}");
        // Nobody has computer use on: a leftover native setting still warns.
        let (level, text) = verdict(&[], Ok("ghcr.io/x/cu:v1"), true, true, &native);
        assert_eq!(level, Level::Warn, "{text}");
        assert!(text.contains("desk-bot"), "{text}");
        // Nobody set to native: no line about it.
        let (level, text) = verdict(&users, Ok("ghcr.io/x/cu:v1"), true, true, &[]);
        assert_eq!(level, Level::Pass, "{text}");
        assert!(!text.contains("native"), "{text}");
    }

    #[test]
    fn user_facing_texts_avoid_the_banned_constructions() {
        let all = [
            missing_message("x:y"),
            unchecked_message("x:y"),
            invalid_config_message("why"),
            native_mode_line(&["a".to_string()]),
            verdict(&["a".to_string()], Ok("x:y"), true, false, &[]).1,
            verdict(&["a".to_string()], Ok("x:y"), true, true, &["a".to_string()]).1,
        ];
        for text in all {
            assert!(!text.contains("——"), "{text}");
            assert!(!(text.contains("不是") && text.contains("而是")), "{text}");
        }
    }
}
