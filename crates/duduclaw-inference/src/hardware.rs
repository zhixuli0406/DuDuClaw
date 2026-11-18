//! Hardware detection for automatic backend selection.
//!
//! R8j round 2 B2: every probe command (`nvidia-smi`, `rocm-smi`, `sysctl`)
//! runs under a hard deadline ([`PROBE_COMMAND_TIMEOUT`]); on expiry the child
//! is killed and reaped and detection continues with the safe default (no GPU
//! detected). A hung GPU tool can no longer hold its caller forever.
//! R8j round 3 F2: on Unix each probe runs in its own process group (its pid
//! is the group id) and the whole group is SIGKILLed, so a helper the tool
//! forked cannot outlive the deadline.

use std::{process::{Output, Stdio}, time::Duration};
use crate::types::{BackendType, GpuType, HardwareInfo};

/// Hard bound for one probe command. A healthy `nvidia-smi` answers in well
/// under a second with the persistence daemon and in a few seconds on a cold
/// multi-GPU driver start; 5 s keeps that margin while bounding a hang. At
/// most two probe commands run per detection (GPU name, then VRAM or the AMD
/// tool), so one detection takes at most 2 × (5 s + [`PROBE_REAP_GRACE`]).
pub const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// After the kill, how long the exit is awaited (which reaps the child). Only
/// a process stuck in an uninterruptible kernel wait outlives SIGKILL longer;
/// it is then left to tokio's orphan reaper (`kill_on_drop`), which reaps it
/// as soon as it exits.
pub const PROBE_REAP_GRACE: Duration = Duration::from_secs(1);
/// At most this much of a probe's stdout is kept.
const PROBE_OUTPUT_LIMIT: u64 = 64 * 1024;

/// Builds one probe command (`program` is a fixed platform tool); the seam
/// tests replace.
type ProbeCommand = fn(&'static str) -> tokio::process::Command;

/// Production probe command: the named tool under the namespace guard.
fn guarded_command(program: &'static str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    duduclaw_core::ns_guard::guard_command(&mut cmd);
    cmd
}

/// How one bounded probe ended.
#[derive(Debug)]
enum Probe {
    Finished(Output),
    /// Could not start, or failed while running (killed and reaped).
    Failed,
    /// The deadline passed: killed; `reaped` once its exit was collected.
    TimedOut { pid: Option<u32>, reaped: bool },
}

#[derive(Debug, PartialEq, Eq)]
enum LeaderState {
    Unreaped,
    Reaped,
}

impl LeaderState {
    fn mark_reaped(&mut self) { *self = Self::Reaped; }
}

/// Runs `cmd` (stdin closed, stderr discarded, stdout captured up to
/// [`PROBE_OUTPUT_LIMIT`]) for at most `limit`. Starts one child process, on
/// Unix as the leader of a new process group (`process_group(0)`: group id =
/// its pid), which everything it forks inherits.
async fn run_bounded(mut cmd: tokio::process::Command, limit: Duration) -> Probe {
    // Signal the process group only while its leader is still unreaped.
    use tokio::io::AsyncReadExt;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let Ok(mut child) = cmd.spawn() else { return Probe::Failed };
    let pid = child.id();
    let mut leader = LeaderState::Unreaped;
    let deadline = tokio::time::Instant::now() + limit;
    let Some(mut stdout) = child.stdout.take() else {
        kill_and_reap(&mut child, pid, &mut leader).await;
        return Probe::Failed;
    };
    let ran = tokio::time::timeout_at(deadline, async {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stdout.read(&mut buffer).await?;
            if read == 0 { break; }
            let keep = read.min(PROBE_OUTPUT_LIMIT as usize - bytes.len());
            bytes.extend_from_slice(&buffer[..keep]);
        }
        // Only wait after EOF, under the same deadline (the remaining time).
        let status = child.wait().await?;
        leader.mark_reaped();
        // A helper outliving a normally exited probe is left alone: after
        // reaping, its former group id must never be signalled.
        Ok::<_, std::io::Error>(Output { status, stdout: bytes, stderr: Vec::new() })
    }).await;
    match ran {
        Ok(Ok(output)) => Probe::Finished(output),
        Ok(Err(_)) => { kill_and_reap(&mut child, pid, &mut leader).await; Probe::Failed }
        Err(_) => { let reaped = kill_and_reap(&mut child, pid, &mut leader).await; Probe::TimedOut { pid, reaped } }
    }
}

/// SIGKILL to the probe's whole process group (Unix; `group` is the pid
/// recorded at spawn, which `process_group(0)` made the group id), then to
/// the direct child, then collect the child's exit (reaping it) for at most
/// [`PROBE_REAP_GRACE`]. An unreaped leader keeps its pid allocated; refuse
/// cleanup after reaping, when the former group id could have been reused.
async fn kill_and_reap(child: &mut tokio::process::Child, group: Option<u32>, leader: &mut LeaderState) -> bool {
    if *leader == LeaderState::Reaped { return true; }
    #[cfg(all(test, unix))]
    let _ = tests::GROUP_KILLS.try_with(|count| {
        assert_eq!(*leader, LeaderState::Unreaped);
        count.set(count.get() + 1);
    });
    #[cfg(unix)]
    if let Some(group) = group.and_then(|pid| libc::pid_t::try_from(pid).ok()).filter(|pid| *pid > 1) {
        // SAFETY: kill takes scalar arguments only; a negative pid addresses
        // the process group `group` (killpg).
        unsafe { libc::kill(-group, libc::SIGKILL); }
    }
    #[cfg(not(unix))]
    let _ = group;
    let _ = child.start_kill();
    let reaped = matches!(tokio::time::timeout(PROBE_REAP_GRACE, child.wait()).await, Ok(Ok(_)));
    if reaped { leader.mark_reaped(); }
    reaped
}

/// [`run_bounded`]'s output, or `None` (not started, failed or timed out).
async fn bounded_output(cmd: tokio::process::Command, limit: Duration) -> Option<Output> {
    match run_bounded(cmd, limit).await {
        Probe::Finished(output) => Some(output),
        Probe::Failed => None,
        Probe::TimedOut { pid, reaped } => {
            tracing::warn!(pid = ?pid, reaped, limit_ms = limit.as_millis() as u64,
                "hardware probe command exceeded its deadline and was killed; continuing without it");
            None
        }
    }
}

/// Detect available hardware and recommend the best backend.
pub async fn detect_hardware() -> HardwareInfo {
    let (gpu_type, gpu_name) = detect_gpu().await;
    let (ram_total_mb, ram_available_mb) = detect_ram();
    let (vram_total_mb, vram_available_mb) = detect_vram(&gpu_type, ram_total_mb, ram_available_mb).await;
    let cpu_cores = std::thread::available_parallelism()
        .map(|p| p.get() as u32)
        .unwrap_or(4);

    let recommended_backend = recommend_backend(&gpu_type);
    // Recommend using at most 70% of available memory for model
    let usable_mb = if gpu_type == GpuType::AppleSilicon {
        // Unified memory — use RAM total
        ram_available_mb * 7 / 10
    } else if vram_total_mb > 0 {
        vram_available_mb * 7 / 10
    } else {
        ram_available_mb * 7 / 10
    };
    let recommended_max_model_gb = usable_mb as f64 / 1024.0;

    HardwareInfo {
        gpu_type,
        gpu_name,
        vram_total_mb,
        vram_available_mb,
        ram_total_mb,
        ram_available_mb,
        cpu_cores,
        recommended_backend,
        recommended_max_model_gb,
    }
}

/// The only backend DuDuClaw actually ships is the OpenAI-compatible HTTP
/// client, so hardware detection no longer picks between in-process engines —
/// it reports the GPU (which still drives `recommended_max_model_gb` and the
/// model-registry hints) and recommends running a local OpenAI-compatible
/// server (llama-server / Ollama / vLLM) on it. The llama.cpp / mistral.rs
/// recommendations were removed on 2026-09-29 together with those backends
/// (`wiki/reports/feature-audit-2026-09-29.md` T1-D3 / T3-S5); recommending a
/// backend that always fails to build is worse than recommending none.
fn recommend_backend(_gpu: &GpuType) -> BackendType {
    BackendType::OpenAiCompat
}

async fn detect_gpu() -> (GpuType, String) {
    // Apple Silicon detection
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        let name = detect_apple_chip().await;
        return (GpuType::AppleSilicon, name);
    }
    detect_discrete_gpu(guarded_command, PROBE_COMMAND_TIMEOUT).await
}

/// NVIDIA, then AMD; a tool that is missing, fails or exceeds `limit` counts
/// as no such GPU (default: CPU only).
async fn detect_discrete_gpu(command: ProbeCommand, limit: Duration) -> (GpuType, String) {
    // NVIDIA detection
    if let Some(name) = detect_nvidia(command, limit).await {
        return (GpuType::NvidiaCuda, name);
    }

    // AMD detection
    if let Some(name) = detect_amd(command, limit).await {
        return (GpuType::AmdRocm, name);
    }

    (GpuType::None, "CPU only".to_string())
}

async fn detect_apple_chip() -> String {
    let mut cmd = tokio::process::Command::new("sysctl");
    cmd.args(["-n", "machdep.cpu.brand_string"]);

    match bounded_output(cmd, PROBE_COMMAND_TIMEOUT).await {
        Some(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => "Apple Silicon".to_string(),
    }
}

async fn detect_nvidia(command: ProbeCommand, limit: Duration) -> Option<String> {
    let mut cmd = command("nvidia-smi");
    cmd.args(["--query-gpu=name", "--format=csv,noheader,nounits"]);
    let output = bounded_output(cmd, limit).await?;

    if !output.status.success() {
        return None;
    }

    let name = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("NVIDIA GPU")
        .trim()
        .to_string();
    Some(name)
}

async fn detect_amd(command: ProbeCommand, limit: Duration) -> Option<String> {
    // Try rocm-smi first
    let mut cmd = command("rocm-smi");
    cmd.args(["--showproductname"]);
    let output = bounded_output(cmd, limit).await;

    if let Some(o) = output
        && o.status.success() {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = text.lines().find(|l| l.contains("GPU")) {
                return Some(line.trim().to_string());
            }
            return Some("AMD GPU (ROCm)".to_string());
        }

    None
}

fn detect_ram() -> (u64, u64) {
    #[cfg(target_os = "macos")]
    {
        detect_ram_macos()
    }
    #[cfg(target_os = "linux")]
    {
        detect_ram_linux()
    }
    #[cfg(target_os = "windows")]
    {
        detect_ram_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        (0, 0)
    }
}

#[cfg(target_os = "macos")]
fn detect_ram_macos() -> (u64, u64) {
    use std::process::Command;

    let total = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok())
        .unwrap_or(0)
        / (1024 * 1024);

    // vm_stat for available memory
    let available = Command::new("vm_stat")
        .output()
        .ok()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            let page_size: u64 = 16384; // ARM64 default
            let free_pages = parse_vm_stat_field(&text, "Pages free");
            let inactive_pages = parse_vm_stat_field(&text, "Pages inactive");
            (free_pages + inactive_pages) * page_size / (1024 * 1024)
        })
        .unwrap_or(total / 2);

    (total, available)
}

#[cfg(target_os = "macos")]
fn parse_vm_stat_field(text: &str, field: &str) -> u64 {
    text.lines()
        .find(|l| l.contains(field))
        .and_then(|l| {
            l.split(':')
                .nth(1)
                .map(|v| v.trim().trim_end_matches('.'))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn detect_ram_linux() -> (u64, u64) {
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();

    let total = parse_meminfo_kb(&meminfo, "MemTotal") / 1024;
    let available = parse_meminfo_kb(&meminfo, "MemAvailable") / 1024;

    (total, available)
}

#[cfg(target_os = "linux")]
fn parse_meminfo_kb(text: &str, field: &str) -> u64 {
    text.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| {
            l.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

#[cfg(target_os = "windows")]
fn detect_ram_windows() -> (u64, u64) {
    use std::process::Command;

    // Use PowerShell to query total and available physical memory.
    // This avoids adding windows-sys as a dependency to this crate.
    let total = Command::new("powershell")
        .args(["-NoProfile", "-Command",
            "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok()
        })
        .unwrap_or(0)
        / (1024 * 1024);

    let available = Command::new("powershell")
        .args(["-NoProfile", "-Command",
            "(Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory"])
        .output()
        .ok()
        .and_then(|o| {
            // FreePhysicalMemory is in KB
            String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok()
        })
        .unwrap_or(0)
        / 1024; // KB → MB

    if total == 0 {
        // Fallback: try wmic (deprecated but widely available)
        let wmic_total = Command::new("wmic")
            .args(["computersystem", "get", "TotalPhysicalMemory", "/value"])
            .output()
            .ok()
            .and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout);
                text.lines()
                    .find(|l| l.starts_with("TotalPhysicalMemory="))
                    .and_then(|l| l.split('=').nth(1))
                    .and_then(|v| v.trim().parse::<u64>().ok())
            })
            .unwrap_or(0)
            / (1024 * 1024);

        let wmic_available = Command::new("wmic")
            .args(["os", "get", "FreePhysicalMemory", "/value"])
            .output()
            .ok()
            .and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout);
                text.lines()
                    .find(|l| l.starts_with("FreePhysicalMemory="))
                    .and_then(|l| l.split('=').nth(1))
                    .and_then(|v| v.trim().parse::<u64>().ok())
            })
            .unwrap_or(0)
            / 1024; // KB → MB

        return (wmic_total, wmic_available);
    }

    (total, available)
}

async fn detect_vram(gpu_type: &GpuType, ram_total: u64, ram_available: u64) -> (u64, u64) {
    match gpu_type {
        GpuType::AppleSilicon => (ram_total, ram_available), // Unified memory
        GpuType::NvidiaCuda => detect_nvidia_vram(guarded_command, PROBE_COMMAND_TIMEOUT).await,
        _ => (0, 0),
    }
}

async fn detect_nvidia_vram(command: ProbeCommand, limit: Duration) -> (u64, u64) {
    let mut cmd = command("nvidia-smi");
    cmd.args([
        "--query-gpu=memory.total,memory.free",
        "--format=csv,noheader,nounits",
    ]);

    match bounded_output(cmd, limit).await {
        Some(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = text.lines().next() {
                let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                if parts.len() == 2 {
                    let total = parts[0].parse::<u64>().unwrap_or(0);
                    let free = parts[1].parse::<u64>().unwrap_or(0);
                    return (total, free);
                }
            }
            (0, 0)
        }
        _ => (0, 0),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    const SHORT: Duration = Duration::from_millis(200);

    tokio::task_local! {
        pub(super) static GROUP_KILLS: std::cell::Cell<usize>;
    }

    /// Stands in for any GPU tool: never exits (the tool's own arguments
    /// become `$0…` of the script and are ignored).
    fn hang(_: &'static str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "exec sleep 30"]);
        cmd
    }
    fn answer(_: &'static str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "echo 'Tesla T4'"]);
        cmd
    }
    /// The process still exists (a zombie counts: it was not reaped).
    fn exists(pid: u32) -> bool {
        std::process::Command::new("sh").args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status().is_ok_and(|status| status.success())
    }

    #[tokio::test]
    async fn hung_probe_is_killed_and_reaped_at_the_deadline() {
        let started = Instant::now();
        let probe = run_bounded(hang("nvidia-smi"), SHORT).await;
        let elapsed = started.elapsed();
        let Probe::TimedOut { pid: Some(pid), reaped } = probe else { panic!("not timed out") };
        assert!(reaped, "the killed child's exit was collected");
        assert!(!exists(pid), "no lingering child or zombie");
        assert!(elapsed >= SHORT);
        assert!(elapsed < SHORT + PROBE_REAP_GRACE + Duration::from_secs(1), "{elapsed:?}");
        // A probe that answers in time is returned as is.
        let Probe::Finished(output) = run_bounded(answer("nvidia-smi"), Duration::from_secs(5)).await else { panic!("not finished") };
        assert!(output.status.success());
        assert_eq!(output.stdout, b"Tesla T4\n");
    }

    /// `kill(pid, 0)` answers ESRCH (gone and reaped) within `bound`.
    async fn gone_within(pid: libc::pid_t, bound: Duration) -> bool {
        let until = Instant::now() + bound;
        loop {
            // SAFETY: scalar arguments only; signal 0 only checks existence.
            let rc = unsafe { libc::kill(pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) { return true; }
            if Instant::now() >= until { return false; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// R8j round 3 F2: a probe tool that forked a helper (which also keeps
    /// stdout open) and hangs: at the deadline the whole process group is
    /// killed, neither the shell nor its background `sleep` survives.
    #[tokio::test]
    async fn hung_probe_with_forked_helper_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let (shell_file, helper_file) = (dir.path().join("shell.pid"), dir.path().join("helper.pid"));
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "echo $$ > \"$1\"; sleep 600 & echo $! > \"$2\"; wait", "sh"])
            .arg(&shell_file).arg(&helper_file);
        let limit = Duration::from_millis(500);
        let started = Instant::now();
        let Probe::TimedOut { pid: Some(pid), reaped } = run_bounded(cmd, limit).await else { panic!("not timed out") };
        assert!(started.elapsed() < limit + PROBE_REAP_GRACE + Duration::from_secs(1), "{:?}", started.elapsed());
        assert!(reaped, "the direct child was reaped");
        let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap().trim().parse::<libc::pid_t>().unwrap();
        let (shell, helper) = (read(&shell_file), read(&helper_file));
        assert_eq!(shell as u32, pid, "the shell is the direct child");
        assert_ne!(helper, shell);
        assert!(gone_within(shell, Duration::from_secs(5)).await, "shell {shell} survived");
        assert!(gone_within(helper, Duration::from_secs(5)).await, "forked helper {helper} survived the deadline");
    }

    #[tokio::test]
    async fn exited_leader_with_stdout_helper_is_killed_before_reaping() {
        let dir = tempfile::tempdir().unwrap();
        let helper_file = dir.path().join("helper.pid");
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 600 & echo $! > \"$1\"; exit 0", "sh"]).arg(&helper_file);
        let limit = Duration::from_millis(500);
        let started = Instant::now();
        let probe = GROUP_KILLS.scope(std::cell::Cell::new(0), async {
            let probe = run_bounded(cmd, limit).await;
            // The hook also asserts Unreaped at the group-kill site.
            GROUP_KILLS.with(|count| assert_eq!(count.get(), 1));
            probe
        }).await;
        let Probe::TimedOut { pid: Some(pid), reaped } = probe else { panic!("not timed out") };
        assert!(started.elapsed() >= limit);
        assert!(started.elapsed() < limit + PROBE_REAP_GRACE + Duration::from_secs(1));
        assert!(reaped, "the exited leader was reaped only after the group kill");
        assert!(!exists(pid));
        let helper = std::fs::read_to_string(&helper_file).unwrap().trim().parse::<libc::pid_t>().unwrap();
        assert!(gone_within(helper, Duration::from_secs(5)).await, "helper {helper} survived the deadline");
    }

    #[tokio::test]
    async fn normal_probe_returns_output_without_group_cleanup() {
        GROUP_KILLS.scope(std::cell::Cell::new(0), async {
            let Probe::Finished(output) = run_bounded(answer("nvidia-smi"), Duration::from_secs(5)).await else { panic!("not finished") };
            assert!(output.status.success());
            assert_eq!(output.stdout, b"Tesla T4\n");
            GROUP_KILLS.with(|count| assert_eq!(count.get(), 0));
        }).await;
    }

    #[tokio::test]
    async fn cleanup_refuses_to_signal_a_reaped_leader() {
        let mut cmd = hang("nvidia-smi");
        cmd.process_group(0).kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        GROUP_KILLS.scope(std::cell::Cell::new(0), async {
            // Use a live sentinel so an accidental signal is observable.
            let mut leader = LeaderState::Reaped;
            assert!(kill_and_reap(&mut child, pid, &mut leader).await);
            GROUP_KILLS.with(|count| assert_eq!(count.get(), 0));
            let alive = exists(pid.unwrap());
            leader = LeaderState::Unreaped;
            assert!(kill_and_reap(&mut child, pid, &mut leader).await);
            assert_eq!(leader, LeaderState::Reaped);
            assert!(alive, "Reaped state must refuse all signalling");
        }).await;
    }

    #[tokio::test]
    async fn hung_gpu_tools_default_to_no_gpu_within_the_bound() {
        let started = Instant::now();
        assert_eq!(detect_discrete_gpu(hang, SHORT).await, (GpuType::None, "CPU only".to_string()));
        assert_eq!(detect_nvidia_vram(hang, SHORT).await, (0, 0));
        // Three probes (NVIDIA, AMD, VRAM), each cut at the deadline.
        assert!(started.elapsed() < 3 * (SHORT + PROBE_REAP_GRACE) + Duration::from_secs(1), "{:?}", started.elapsed());
        // Through the same seam, an answering tool is detected.
        assert_eq!(detect_discrete_gpu(answer, Duration::from_secs(5)).await, (GpuType::NvidiaCuda, "Tesla T4".to_string()));
        // The production bound is a few seconds, never unbounded.
        assert_eq!(PROBE_COMMAND_TIMEOUT, Duration::from_secs(5));
    }
}
