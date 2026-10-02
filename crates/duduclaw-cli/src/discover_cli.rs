//! Hidden operator-only discovery entry point. No MCP or channel route.
use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::discovery::{
    DiscoveryStore,
    agent_spawn::AttemptRunnerFactory,
    budget::SharedBudget,
    config::DiscoveryConfig,
    evaluator::RegisteredEvaluator,
    online::{OnlineComponents, RunSpec, RunIdentity},
    policy_runner::{ManagedPolicySource, PythonPolicyRuntime},
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[derive(Debug, Subcommand)]
pub(crate) enum DiscoverCommands {
    Run {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, value_parser = ["container", "native", "none"])]
        attempt_backend: Option<String>,
        #[arg(long)]
        allow_shared_account_pool: bool,
        #[arg(long)]
        strict_usd: bool,
    },
    Evaluator {
        #[command(subcommand)]
        command: EvaluatorCommands,
    },
}
#[derive(Debug, Subcommand)]
pub(crate) enum EvaluatorCommands {
    Register { name: String },
}

fn authorize(identity: Option<&str>) -> Result<()> {
    if identity.is_some() {
        return Err(DuDuClawError::Security(
            "探索指令只能由操作者執行，AI 員工工作階段無法使用。".into(),
        ));
    }
    Ok(())
}
fn config(home: &Path) -> Result<DiscoveryConfig> {
    let text = std::fs::read_to_string(home.join("config.toml"))?;
    let table = text.parse::<toml::Table>()?;
    table
        .get("discovery")
        .ok_or_else(|| DuDuClawError::Config("請先由操作者設定探索的工作區與打分器。".into()))?
        .clone()
        .try_into()
        .map_err(DuDuClawError::TomlDeser)
}
fn execution_lock(home: &Path) -> Result<duduclaw_gateway::discovery::maintenance::OperatorLeaseGuard> {
    duduclaw_gateway::discovery::maintenance::OperatorLeaseGuard::acquire(home)
        .map_err(DuDuClawError::Agent)
}

pub(crate) async fn execute(
    command: DiscoverCommands,
    home: &Path,
    identity: Option<&str>,
) -> Result<()> {
    authorize(identity)?;
    let mut cfg = config(home)?;
    match command {
        DiscoverCommands::Evaluator {
            command: EvaluatorCommands::Register { name },
        } => {
            let lease = execution_lock(home)?;
            duduclaw_gateway::discovery::maintenance::reconcile_with_lease(home, &lease)
                .map_err(DuDuClawError::Agent)?;
            let tested_config = serde_json::to_value(cfg.evaluators.get(&name)
                .ok_or_else(|| DuDuClawError::Config("打分器尚未登錄。".into()))?)?;
            let registry = RegisteredEvaluator::new(home.into(), cfg, true);
            let sha256 = tokio::select! {
                result = registry.register(&name) => result.map_err(DuDuClawError::Agent)?,
                _ = lease.cancelled() => return Err(DuDuClawError::Agent("操作者 lease 已失效，打分器登錄已取消。".into())),
            };
            lease.check_home(home).map_err(DuDuClawError::Agent)?;
            duduclaw_gateway::discovery::maintenance::check_clean(home).map_err(DuDuClawError::Agent)?;
            // Persist only after both known-good and cheating fixtures pass.
            let path = home.join("config.toml");
            duduclaw_core::with_file_lock(&path, || {
                let source = std::fs::read_to_string(&path)?;
                let current_table = source.parse::<toml::Table>().map_err(std::io::Error::other)?;
                let current: DiscoveryConfig = current_table.get("discovery")
                    .ok_or_else(|| std::io::Error::other("discovery configuration removed"))?
                    .clone().try_into().map_err(std::io::Error::other)?;
                let current_config = serde_json::to_value(current.evaluators.get(&name))
                    .map_err(std::io::Error::other)?;
                if current_config != tested_config {
                    return Err(std::io::Error::other("evaluator settings changed during self-test; repeat registration"));
                }
                let mut doc = source
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(std::io::Error::other)?;
                doc["discovery"]["evaluators"][&name]["sha256"] = toml_edit::value(&sha256);
                let temporary = home.join(format!("config.discovery-{}.tmp", uuid::Uuid::new_v4()));
                std::fs::write(&temporary, doc.to_string())?;
                std::fs::rename(temporary, &path)
            })?;
            println!(
                "{}",
                serde_json::json!({"status":"registered","evaluator":name,"sha256":sha256})
            );
        }
        DiscoverCommands::Run { spec: path, attempt_backend, allow_shared_account_pool, strict_usd } => {
            if let Some(backend)=attempt_backend { cfg.attempt.sandbox=match backend.as_str() {"none"=>duduclaw_gateway::discovery::config::AttemptSandbox::None,"native"=>duduclaw_gateway::discovery::config::AttemptSandbox::Native,_=>duduclaw_gateway::discovery::config::AttemptSandbox::Container}; }
            cfg.attempt.allow_shared_account_pool |= allow_shared_account_pool;
            cfg.attempt.strict_usd |= strict_usd;
            let text = std::fs::read_to_string(path)?;
            let spec: RunSpec = toml::from_str(&text)?;
            spec.validate().map_err(DuDuClawError::Config)?;
            let lease = execution_lock(home)?;
            duduclaw_gateway::discovery::maintenance::reconcile_with_lease(home, &lease)
                .map_err(DuDuClawError::Agent)?;
            let runs = home.join("discovery/runs");
            std::fs::create_dir_all(&runs)?;
            duduclaw_gateway::discovery::workspace::cleanup_retained_runs(
                &runs,
                &cfg,
                SystemTime::now(),
            )?;
            DiscoveryStore::open(home)
                .map_err(|e| DuDuClawError::Agent(e.to_string()))?
                .interrupt_running()
                .map_err(|e| DuDuClawError::Agent(e.to_string()))?;
            let scorer = cfg
                .evaluators
                .get(&spec.evaluator)
                .ok_or_else(|| DuDuClawError::Config("打分器尚未登錄。".into()))?
                .sha256
                .clone();
            if scorer.len() != 64 {
                return Err(DuDuClawError::Config("請先完成打分器自測與登錄。".into()));
            }
            let budget = SharedBudget::new(spec.budget).map_err(DuDuClawError::Config)?;
            lease.bind_budget(budget.clone()).map_err(DuDuClawError::Agent)?;
            let mut factory = tokio::select! {
                result = AttemptRunnerFactory::for_run(home.into(), &cfg, budget.clone(), true) =>
                    result.map_err(|e| DuDuClawError::Agent(e.to_string()))?,
                _ = lease.cancelled() => return Err(DuDuClawError::Agent("操作者 lease 已失效，探索準備已取消。".into())),
            };
            factory.max_concurrency = spec.max_parallelism;
            let runner = Arc::from(
                factory
                    .for_runtime(&spec.runtime)
                    .map_err(|e| DuDuClawError::Agent(e.to_string()))?,
            );
            let identity = RunIdentity::operator();
            let source = Arc::new(ManagedPolicySource::new(PythonPolicyRuntime::detect_scoped_with_quota(
                home, &identity.run_id, budget.clone(), duduclaw_gateway::discovery::attempt_container::QuotaLimits {
                    max_run_bytes: cfg.max_run_bytes, max_total_bytes: cfg.max_total_bytes })));
            source.set_online_timeout(Duration::from_secs(spec.budget.max_wall_secs));
            let components = OnlineComponents {
                runner,
                evaluator: Arc::new(RegisteredEvaluator::new(home.into(), cfg.clone(), true)),
                policy: source.clone(),
                dreaming: Some(source),
            };
            let report = duduclaw_gateway::discovery::online::run_with_lease_and_identity(
                home.into(),
                cfg,
                spec,
                components,
                budget,
                scorer,
                lease,
                identity,
            )
            .await
            .map_err(DuDuClawError::Agent)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ai_session_is_refused_before_configuration_or_process_work() {
        assert!(matches!(
            authorize(Some("test")),
            Err(DuDuClawError::Security(_))
        ));
        assert!(authorize(None).is_ok());
    }
    #[tokio::test]
    async fn actual_entry_refuses_ai_identity_even_without_a_home() {
        let result = execute(
            DiscoverCommands::Run {
                spec: PathBuf::from("/nonexistent"), attempt_backend:None, allow_shared_account_pool:false, strict_usd:false,
            },
            Path::new("/nonexistent"),
            Some("agent"),
        )
        .await;
        assert!(matches!(result, Err(DuDuClawError::Security(_))));
    }
    #[test]
    fn discover_does_not_appear_in_top_level_help() {
        use clap::CommandFactory;
        let mut command = crate::Cli::command();
        let mut output = vec![];
        command.write_long_help(&mut output).unwrap();
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .lines()
                .any(|l| l.trim_start().starts_with("discover "))
        );
    }
}
