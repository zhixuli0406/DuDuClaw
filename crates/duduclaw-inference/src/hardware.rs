//! Hardware detection for automatic backend selection.
//!
//! R8j round 2 B2: every probe command (`nvidia-smi`, `rocm-smi`, `sysctl`)
//! runs under a hard deadline ([`PROBE_COMMAND_TIMEOUT`]); on expiry the child
//! is killed and reaped and detection continues with the safe default (no GPU
//! detected). A hung GPU tool can no longer hold its caller forever.

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

/// Runs `cmd` (stdin closed, stderr discarded, stdout captured up to
/// [`PROBE_OUTPUT_LIMIT`]) for at most `limit`. Starts one child process.
async fn run_bounded(mut cmd: tokio::process::Command, limit: Duration) -> Probe {
    use tokio::io::AsyncReadExt;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let Ok(mut child) = cmd.spawn() else { return Probe::Failed };
    let pid = child.id();
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(&mut child).await;
        return Probe::Failed;
    };
    let ran = tokio::time::timeout(limit, async {
        let mut bytes = Vec::new();
        let mut limited = stdout.take(PROBE_OUTPUT_LIMIT);
        let (read, status) = tokio::join!(limited.read_to_end(&mut bytes), child.wait());
        match (read, status) {
            (Ok(_), Ok(status)) => Some(Output { status, stdout: bytes, stderr: Vec::new() }),
            _ => None,
        }
    }).await;
    match ran {
        Ok(Some(output)) => Probe::Finished(output),
        Ok(None) => { kill_and_reap(&mut child).await; Probe::Failed }
        Err(_) => { let reaped = kill_and_reap(&mut child).await; Probe::TimedOut { pid, reaped } }
    }
}

/// SIGKILL, then collect the exit (reaping it) for at most [`PROBE_REAP_GRACE`].
async fn kill_and_reap(child: &mut tokio::process::Child) -> bool {
    let _ = child.start_kill();
    matches!(tokio::time::timeout(PROBE_REAP_GRACE, child.wait()).await, Ok(Ok(_)))
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
