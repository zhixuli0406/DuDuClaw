use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::ContainerRuntime;
use duduclaw_core::types::*;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

/// The script sandbox's default image: the platform's published image for
/// this version (`ghcr.io/zhixuli0406/duduclaw:v<version>`), the same one the
/// task sandbox uses. Overridden by `config.toml [container.sandbox] image`
/// through [`DockerRuntime::with_image`] / [`crate::RuntimeBackend::detect_with_image`].
pub fn default_image() -> String {
    duduclaw_core::sandbox_image::platform_image(env!("CARGO_PKG_VERSION"))
}

/// Memory ceiling of one script-sandbox container (2 GiB).
pub const SCRIPT_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
/// Process ceiling: enough for a script and its helpers, not a fork bomb.
pub const SCRIPT_PIDS_LIMIT: i64 = 256;
/// CPU ceiling in units of 10⁻⁹ CPUs (one CPU).
pub const SCRIPT_NANO_CPUS: i64 = 1_000_000_000;
/// `/tmp` scratch space.
pub const SCRIPT_TMPFS_OPTIONS: &str = "rw,noexec,nosuid,nodev,size=64m";
/// The json-file log is capped on disk (`max-size`, one file) …
pub const SCRIPT_LOG_MAX_SIZE: &str = "8m";
pub const SCRIPT_LOG_MAX_FILES: &str = "1";
/// … and at most this many bytes of it are read back into memory.
pub const SCRIPT_LOG_READ_MAX_BYTES: usize = 2 * 1024 * 1024;

pub struct DockerRuntime {
    client: Docker,
    image: String,
}

impl DockerRuntime {
    /// Connect to the local Docker daemon, running [`default_image`].
    pub fn new() -> Result<Self> {
        Self::with_image(&default_image())
    }

    /// Connect to the local Docker daemon, running `image`. The image is
    /// validated here and never pulled: a missing image fails `create` with a
    /// message naming `docker pull <image>`.
    pub fn with_image(image: &str) -> Result<Self> {
        let image = image.trim();
        if !duduclaw_core::sandbox_image::valid_image(image) {
            return Err(DuDuClawError::Container(format!(
                "invalid sandbox image reference {image:?}"
            )));
        }
        let client = Docker::connect_with_local_defaults().map_err(|e| {
            DuDuClawError::Container(format!("Failed to connect to Docker daemon: {e}"))
        })?;
        Ok(Self {
            client,
            image: image.to_string(),
        })
    }

    /// The image this runtime creates containers from.
    pub fn image(&self) -> &str {
        &self.image
    }

    /// Whether the image is present locally (never pulls). `Err` when the
    /// daemon could not answer.
    pub async fn image_present(&self) -> Result<bool> {
        match self.client.inspect_image(&self.image).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => Ok(false),
            Err(e) => Err(DuDuClawError::Container(format!("cannot inspect sandbox image {}: {e}", self.image))),
        }
    }

    /// A clone of the daemon client, for the remove-on-drop guard.
    pub(crate) fn client(&self) -> Docker {
        self.client.clone()
    }
}

/// Build the container spec for one script-sandbox run.
///
/// The platform image's `ENTRYPOINT` is the gateway's own entrypoint script,
/// so the image default can never be used: the first element of
/// `config.cmd` becomes the entrypoint and the rest its arguments, and an
/// empty `cmd` is refused. The container runs as
/// [`duduclaw_core::sandbox_image::script_sandbox_user`] (never root).
/// Programs are looked up through the image's `PATH`, so the interpreter
/// location (`/usr/local/bin/python3` in the platform image, `/usr/bin/python3`
/// in a distro image) is not hard-coded here.
fn build_container_config(
    image: &str,
    config: &ContainerConfig,
    host_config: bollard::models::HostConfig,
    labels: HashMap<String, String>,
) -> Result<Config<String>> {
    let Some((program, args)) = config.cmd.split_first() else {
        return Err(DuDuClawError::Container(
            "script sandbox needs an explicit command (the image's default entrypoint is the gateway)"
                .to_string(),
        ));
    };
    let env = if config.env.is_empty() {
        None
    } else {
        Some(
            config
                .env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<String>>(),
        )
    };
    Ok(Config {
        image: Some(image.to_string()),
        labels: Some(labels),
        stop_timeout: Some((config.timeout_ms / 1000) as i64),
        host_config: Some(host_config),
        entrypoint: Some(vec![program.clone()]),
        cmd: Some(args.to_vec()),
        user: Some(duduclaw_core::sandbox_image::script_sandbox_user()),
        working_dir: Some("/tmp".to_string()),
        env,
        ..Default::default()
    })
}

/// Build the bind-mount strings (`host:container:mode`) for a container config.
fn build_binds(config: &ContainerConfig) -> Vec<String> {
    config
        .additional_mounts
        .iter()
        .map(|mount| {
            let mode = if mount.readonly { "ro" } else { "rw" };
            format!("{}:{}:{}", mount.host, mount.container, mode)
        })
        .collect()
}

/// Build the `HostConfig` that applies sandbox isolation for a container.
///
/// C6 / C6.6: this is the single source of truth for isolation so it can be
/// unit-tested without a Docker daemon. Isolation rules:
///  - network: `none` unless the agent explicitly opted into egress;
///  - tmpfs: a small writable `/tmp` so a readonly-rootfs container still has
///    scratch space;
///  - memory / pids / CPU ceilings so a runaway task can't exhaust the host;
///  - a size-capped json-file log (the output is read back from it).
fn build_host_config(config: &ContainerConfig) -> bollard::models::HostConfig {
    let binds = build_binds(config);

    let network_mode = if config.network_access {
        None // Docker default bridge — egress allowed (explicit opt-in)
    } else {
        Some("none".to_string())
    };

    let mut tmpfs = HashMap::new();
    tmpfs.insert("/tmp".to_string(), SCRIPT_TMPFS_OPTIONS.to_string());
    let log_config = bollard::models::HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(HashMap::from([
            ("max-size".to_string(), SCRIPT_LOG_MAX_SIZE.to_string()),
            ("max-file".to_string(), SCRIPT_LOG_MAX_FILES.to_string()),
        ])),
    };

    bollard::models::HostConfig {
        binds: if binds.is_empty() { None } else { Some(binds) },
        readonly_rootfs: if config.readonly_project {
            Some(true)
        } else {
            None
        },
        network_mode,
        tmpfs: Some(tmpfs),
        // 2 GiB cap — bounds runaway tasks without breaking typical builds.
        memory: Some(SCRIPT_MEMORY_BYTES),
        memory_swap: Some(SCRIPT_MEMORY_BYTES),
        pids_limit: Some(SCRIPT_PIDS_LIMIT),
        nano_cpus: Some(SCRIPT_NANO_CPUS),
        log_config: Some(log_config),
        // A script needs no capability, and must not regain any via setuid
        // binaries the (general-purpose platform) image happens to carry.
        cap_drop: Some(vec!["ALL".to_string()]),
        security_opt: Some(vec!["no-new-privileges".to_string()]),
        ..Default::default()
    }
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        let container_name = format!("duduclaw-{}", uuid::Uuid::new_v4());

        // Never pull: an operator decides which image runs. A missing image
        // makes the sandbox unavailable, and the error names the remedy.
        match self.client.inspect_image(&self.image).await {
            Ok(_) => {}
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => {
                return Err(DuDuClawError::Container(
                    duduclaw_core::sandbox_image::image_missing_message(&self.image),
                ));
            }
            Err(e) => {
                return Err(DuDuClawError::Container(format!(
                    "cannot inspect sandbox image {}: {e}",
                    self.image
                )));
            }
        }

        let mut labels = HashMap::new();
        labels.insert("managed-by".to_string(), "duduclaw".to_string());

        // C6 fix: actually apply sandbox isolation (network=none / tmpfs / memory)
        // via the shared, unit-tested `build_host_config` helper. Previously only
        // binds + readonly_rootfs were set, so the container ran on Docker's
        // default bridge with full egress despite `network_access=false`.
        let host_config = build_host_config(&config);

        if config.network_access {
            warn!(
                name = %container_name,
                "sandbox container created WITH network egress (network_access=true)"
            );
        } else {
            info!(name = %container_name, "sandbox network isolation: none");
        }

        // HC5: run the requested command with the requested env vars (the
        // PTC path passes none: it mounts only its read-only script scratch
        // directory and there is no RPC socket). The command overrides the
        // image entrypoint.
        let container_config = build_container_config(&self.image, &config, host_config, labels)?;

        let options = CreateContainerOptions {
            name: container_name.as_str(),
            platform: None,
        };

        let response = self
            .client
            .create_container(Some(options), container_config)
            .await
            .map_err(|e| DuDuClawError::Container(format!("Failed to create container: {}", e)))?;

        info!(id = %response.id, name = %container_name, "Container created");
        Ok(ContainerId(response.id))
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        self.client
            .start_container(&id.0, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| DuDuClawError::Container(format!("Failed to start container: {}", e)))?;

        info!(id = %id.0, "Container started");
        Ok(())
    }

    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()> {
        let options = StopContainerOptions {
            t: timeout.as_secs() as i64,
        };

        self.client
            .stop_container(&id.0, Some(options))
            .await
            .map_err(|e| DuDuClawError::Container(format!("Failed to stop container: {}", e)))?;

        info!(id = %id.0, "Container stopped");
        Ok(())
    }

    async fn remove(&self, id: &ContainerId) -> Result<()> {
        let options = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };

        self.client
            .remove_container(&id.0, Some(options))
            .await
            .map_err(|e| {
                DuDuClawError::Container(format!("Failed to remove container: {}", e))
            })?;

        info!(id = %id.0, "Container removed");
        Ok(())
    }

    async fn logs(&self, id: &ContainerId) -> Result<String> {
        let options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow: false,
            ..Default::default()
        };

        let mut stream = self.client.logs(&id.0, Some(options));
        let mut output = String::new();

        // Bounded: stop reading once the cap is reached, cut on a char
        // boundary, and mark the cut.
        while let Some(result) = stream.next().await {
            match result {
                Ok(chunk) => {
                    output.push_str(&chunk.to_string());
                    if output.len() > SCRIPT_LOG_READ_MAX_BYTES {
                        output = duduclaw_core::truncate_bytes(&output, SCRIPT_LOG_READ_MAX_BYTES).to_string();
                        output.push_str("\n...[output truncated]");
                        break;
                    }
                }
                Err(e) => {
                    warn!(id = %id.0, error = %e, "Error reading container logs");
                    break;
                }
            }
        }

        Ok(output)
    }

    async fn wait(&self, id: &ContainerId) -> Result<ContainerExit> {
        // Wait for the container to exit, capturing the status code. The
        // wait_container stream yields one response on exit; bollard turns a
        // non-zero exit into a `DockerContainerWaitError` carrying the code.
        let mut stream = self
            .client
            .wait_container(&id.0, None::<WaitContainerOptions<String>>);

        let mut exit_code: i64 = 0;
        while let Some(result) = stream.next().await {
            match result {
                Ok(resp) => exit_code = resp.status_code,
                Err(bollard::errors::Error::DockerContainerWaitError { code, .. }) => {
                    exit_code = code;
                }
                Err(e) => {
                    return Err(DuDuClawError::Container(format!(
                        "Failed to wait for container: {e}"
                    )));
                }
            }
        }

        // Collect logs after exit so we get the full output.
        let logs = self.logs(id).await.unwrap_or_default();

        info!(id = %id.0, exit_code, "Container exited");
        Ok(ContainerExit { exit_code, logs })
    }

    async fn health_check(&self) -> Result<RuntimeHealth> {
        match self.client.ping().await {
            Ok(_) => {
                // Get system info for uptime
                let info = self.client.info().await.map_err(|e| {
                    DuDuClawError::Container(format!("Failed to get Docker info: {}", e))
                })?;

                let containers_running = info.containers_running.unwrap_or(0) as u64;

                Ok(RuntimeHealth {
                    healthy: true,
                    message: format!(
                        "Docker daemon is healthy, {} containers running",
                        containers_running
                    ),
                    uptime_seconds: 0, // Docker API does not expose daemon uptime directly
                })
            }
            Err(e) => Ok(RuntimeHealth {
                healthy: false,
                message: format!("Docker daemon unreachable: {}", e),
                uptime_seconds: 0,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::MountConfig;

    /// Minimal `ContainerConfig` for isolation tests.
    fn base_config(network_access: bool) -> ContainerConfig {
        ContainerConfig {
            timeout_ms: 30_000,
            max_concurrent: 1,
            readonly_project: true,
            additional_mounts: vec![MountConfig {
                host: "/host/work".to_string(),
                container: "/workspace".to_string(),
                readonly: true,
            }],
            sandbox_enabled: true,
            network_access,
            cmd: vec![],
            env: vec![],
        }
    }

    #[test]
    fn build_host_config_isolates_when_network_disabled() {
        // C6.6: network_access=false ⇒ --network=none + tmpfs + memory cap.
        let hc = build_host_config(&base_config(false));

        assert_eq!(
            hc.network_mode.as_deref(),
            Some("none"),
            "network must be `none` when network_access=false"
        );
        let tmpfs = hc.tmpfs.expect("tmpfs must be set");
        assert!(tmpfs.contains_key("/tmp"), "/tmp tmpfs mount must be present");
        let opts: Vec<&str> = tmpfs["/tmp"].split(',').collect();
        for flag in ["noexec", "nosuid", "nodev"] {
            assert!(opts.contains(&flag), "/tmp tmpfs must be {flag}: {opts:?}");
        }
        assert_eq!(
            hc.memory,
            Some(2 * 1024 * 1024 * 1024),
            "memory cap must be applied"
        );
        assert_eq!(hc.readonly_rootfs, Some(true));
        assert_eq!(hc.pids_limit, Some(SCRIPT_PIDS_LIMIT));
        assert_eq!(hc.nano_cpus, Some(SCRIPT_NANO_CPUS));
        assert_eq!(hc.memory_swap, Some(SCRIPT_MEMORY_BYTES));
        let log = hc.log_config.clone().expect("log config");
        assert_eq!(log.typ.as_deref(), Some("json-file"));
        let log = log.config.expect("log options");
        assert_eq!(log.get("max-size").map(String::as_str), Some(SCRIPT_LOG_MAX_SIZE));
        assert_eq!(log.get("max-file").map(String::as_str), Some("1"));
        assert_eq!(hc.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(hc.security_opt, Some(vec!["no-new-privileges".to_string()]));
        // The single additional mount must be rendered as a bind string.
        let binds = hc.binds.expect("binds must be present");
        assert_eq!(binds, vec!["/host/work:/workspace:ro".to_string()]);
    }

    #[test]
    fn build_host_config_allows_egress_when_network_enabled() {
        // network_access=true ⇒ network_mode None (Docker default bridge),
        // but tmpfs + memory isolation still apply.
        let hc = build_host_config(&base_config(true));

        assert!(
            hc.network_mode.is_none(),
            "network_mode must be None (default bridge) when network_access=true"
        );
        assert!(hc.tmpfs.is_some(), "tmpfs still applied with egress");
        assert_eq!(hc.memory, Some(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn build_binds_renders_read_write_mode() {
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![MountConfig {
            host: "/run/duduclaw".to_string(),
            container: "/run/duduclaw".to_string(),
            readonly: false,
        }];
        let binds = build_binds(&cfg);
        assert_eq!(binds, vec!["/run/duduclaw:/run/duduclaw:rw".to_string()]);
    }

    #[test]
    fn default_image_is_the_published_platform_image_for_this_version() {
        assert_eq!(
            default_image(),
            format!("ghcr.io/zhixuli0406/duduclaw:v{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn container_spec_overrides_the_entrypoint_and_never_runs_as_root() {
        let mut cfg = base_config(false);
        cfg.cmd = vec!["python3".into(), "/workspace/script.py".into()];
        cfg.env = vec![("EXAMPLE_VAR".into(), "value".into())];
        let spec = build_container_config(
            "ghcr.io/x/y:v1",
            &cfg,
            build_host_config(&cfg),
            HashMap::new(),
        )
        .unwrap();
        assert_eq!(spec.image.as_deref(), Some("ghcr.io/x/y:v1"));
        // The interpreter is found through the image PATH, not a fixed path.
        assert_eq!(spec.entrypoint, Some(vec!["python3".to_string()]));
        assert_eq!(spec.cmd, Some(vec!["/workspace/script.py".to_string()]));
        assert_eq!(
            spec.env,
            Some(vec!["EXAMPLE_VAR=value".to_string()])
        );
        let user = spec.user.expect("user is always set");
        assert!(!user.starts_with("0:") && user != "0", "{user}");
        assert_eq!(spec.stop_timeout, Some(30));
        let hc = spec.host_config.expect("host config");
        assert_eq!(hc.network_mode.as_deref(), Some("none"));
    }

    #[test]
    fn container_spec_refuses_an_empty_command() {
        // The platform image's default entrypoint is the gateway: never run it.
        let cfg = base_config(false);
        let err = build_container_config("img", &cfg, build_host_config(&cfg), HashMap::new())
            .unwrap_err();
        assert!(err.to_string().contains("explicit command"), "{err}");
    }

    #[test]
    fn with_image_rejects_an_option_like_reference() {
        // Validation happens before the daemon is contacted.
        let err = DockerRuntime::with_image("--privileged").err().expect("rejected");
        assert!(err.to_string().contains("invalid sandbox image"), "{err}");
    }

    /// The image the real-Docker tests run: `DUDU_TASK_SANDBOX_IMAGE` if set,
    /// else the published platform image for this version.
    fn test_image() -> String {
        std::env::var("DUDU_TASK_SANDBOX_IMAGE").unwrap_or_else(|_| default_image())
    }

    /// Integration test — requires a running Docker daemon and the sandbox
    /// image present locally (`DUDU_TASK_SANDBOX_IMAGE`, else the published
    /// platform image; never pulled). Asserts that a `--network=none`
    /// container genuinely has no egress. Marked `#[ignore]` so CI without
    /// Docker skips it.
    ///
    /// Run manually with: `cargo test -p duduclaw-container -- --ignored network_none`
    #[tokio::test]
    #[ignore = "requires a Docker daemon + the sandbox image present locally"]
    async fn network_none_blocks_egress() {
        let runtime = DockerRuntime::with_image(&test_image()).expect("Docker daemon must be reachable");

        // Try to reach an external host; with --network=none this must fail.
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![];
        cfg.cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            // exit 0 only if egress succeeds — we assert the opposite.
            "getent hosts example.com >/dev/null 2>&1 && echo EGRESS_OK || echo EGRESS_BLOCKED"
                .to_string(),
        ];

        let id = runtime.create(cfg).await.expect("create container");
        runtime.start(&id).await.expect("start container");
        let exit = runtime.wait(&id).await.expect("wait container");
        let _ = runtime.remove(&id).await;

        assert!(
            exit.logs.contains("EGRESS_BLOCKED"),
            "container with --network=none must not resolve external hosts; logs: {}",
            exit.logs
        );
    }

    /// A missing image is reported with the `docker pull` remedy and is not
    /// pulled. Requires a Docker daemon (no image needed).
    #[tokio::test]
    #[ignore = "requires a Docker daemon"]
    async fn script_sandbox_missing_image_names_docker_pull() {
        let image = "duduclaw-test-image-that-does-not-exist:v0.0.0";
        let runtime = DockerRuntime::with_image(image).expect("Docker daemon must be reachable");
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![];
        cfg.cmd = vec!["true".into()];
        let err = runtime.create(cfg).await.err().expect("missing image must fail");
        assert!(err.to_string().contains(&format!("docker pull {image}")), "{err}");
    }

    fn managed_containers() -> usize {
        let out = std::process::Command::new("docker")
            .args(["ps", "-aq", "--filter", "label=managed-by=duduclaw"])
            .output()
            .expect("docker CLI");
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()).count()
    }

    fn sleeper(secs: u32) -> ContainerConfig {
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![];
        cfg.cmd = vec!["sleep".into(), secs.to_string()];
        cfg
    }

    /// B5: the hard timeout ends a run that would outlive its deadline, and
    /// the container is gone afterwards.
    #[tokio::test]
    #[ignore = "requires a Docker daemon + the sandbox image present locally"]
    async fn script_sandbox_run_once_times_out_and_removes_the_container() {
        let runtime = crate::RuntimeBackend::Docker(DockerRuntime::with_image(&test_image()).expect("docker"));
        let before = managed_containers();
        let started = std::time::Instant::now();
        let result = runtime.run_once(sleeper(60), Duration::from_secs(2)).await;
        assert!(matches!(result, Err(crate::RunFailure::TimedOut)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(30));
        assert_eq!(managed_containers(), before, "container removed");
    }

    /// B5: a caller whose future is dropped mid-run (cancellation) does not
    /// leave the container running.
    #[tokio::test]
    #[ignore = "requires a Docker daemon + the sandbox image present locally"]
    async fn script_sandbox_cancelled_run_is_removed_by_the_guard() {
        let runtime = std::sync::Arc::new(crate::RuntimeBackend::Docker(
            DockerRuntime::with_image(&test_image()).expect("docker"),
        ));
        let before = managed_containers();
        let task = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.run_once(sleeper(120), Duration::from_secs(300)).await })
        };
        // Wait until the container exists, then cancel the caller.
        for _ in 0..100 {
            if managed_containers() > before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(managed_containers() > before, "container started");
        task.abort();
        let _ = task.await;
        for _ in 0..100 {
            if managed_containers() == before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(managed_containers(), before, "the drop guard removed the container");
    }

    /// B4: output beyond the read cap is cut on a char boundary.
    #[tokio::test]
    #[ignore = "requires a Docker daemon + the sandbox image present locally"]
    async fn script_sandbox_log_read_is_bounded() {
        let runtime = crate::RuntimeBackend::Docker(DockerRuntime::with_image(&test_image()).expect("docker"));
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![];
        // ~3 MiB of three-byte characters, more than the read cap.
        cfg.cmd = vec!["sh".into(), "-c".into(), "i=0; while [ $i -lt 30000 ]; do echo 字字字字字字字字字字字字字字字字字字字字字字字字字字字字字字字字; i=$((i+1)); done".into()];
        let exit = runtime.run_once(cfg, Duration::from_secs(120)).await.expect("run");
        assert!(exit.logs.len() <= SCRIPT_LOG_READ_MAX_BYTES + 64, "{}", exit.logs.len());
        assert!(exit.logs.ends_with("[output truncated]"));
    }
}
