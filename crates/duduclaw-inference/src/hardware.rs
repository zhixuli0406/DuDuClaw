//! Hardware detection for automatic backend selection.

use crate::types::{BackendType, GpuType, HardwareInfo};

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

fn recommend_backend(gpu: &GpuType) -> BackendType {
    match gpu {
        GpuType::AppleSilicon => BackendType::LlamaCpp, // Metal backend
        GpuType::NvidiaCuda => BackendType::LlamaCpp,   // CUDA backend
        GpuType::AmdRocm | GpuType::Vulkan => BackendType::LlamaCpp, // Vulkan backend
        GpuType::IntelArc => BackendType::LlamaCpp,     // SYCL or Vulkan
        GpuType::None => BackendType::LlamaCpp,         // CPU fallback
    }
}

async fn detect_gpu() -> (GpuType, String) {
    // Apple Silicon detection
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        let name = detect_apple_chip().await;
        return (GpuType::AppleSilicon, name);
    }

    // NVIDIA detection
    if let Some(name) = detect_nvidia().await {
        return (GpuType::NvidiaCuda, name);
    }

    // AMD detection
    if let Some(name) = detect_amd().await {
        return (GpuType::AmdRocm, name);
    }

    (GpuType::None, "CPU only".to_string())
}

async fn detect_apple_chip() -> String {
    let output = tokio::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => "Apple Silicon".to_string(),
    }
}

async fn detect_nvidia() -> Option<String> {
    let output = tokio::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader,nounits"])
        .output()
        .await
        .ok()?;

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

async fn detect_amd() -> Option<String> {
    // Try rocm-smi first
    let output = tokio::process::Command::new("rocm-smi")
        .args(["--showproductname"])
        .output()
        .await;

    if let Ok(o) = output
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
        GpuType::NvidiaCuda => detect_nvidia_vram().await,
        _ => (0, 0),
    }
}

async fn detect_nvidia_vram() -> (u64, u64) {
    let output = tokio::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
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
