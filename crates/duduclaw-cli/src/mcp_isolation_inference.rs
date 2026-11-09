//! Fixed host-authority inference observations for authenticated MCP calls.
//! No employee-selected executable, path, credential, HTTP client or cache.
use std::{io::Read, path::Path};
use duduclaw_mcp_boundary::{RequestContext, VerificationError, tool_policy};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_KERNEL_TEXT: usize = 64 * 1024;
const MAX_QUERY_BYTES: usize = 512;

pub(crate) fn supports(name: &str) -> bool {
    cfg!(target_os = "linux") && matches!(name, "hardware_info" | "model_search" | "model_recommend")
}

pub(crate) fn describe(tool: &mut Value) {
    match tool["name"].as_str() {
        Some("hardware_info") => tool["description"] = json!(
            "Read a bounded host RAM/CPU and PCI GPU snapshot without external probes. GPU model names and unsupported VRAM counters remain unknown."),
        Some("model_search") => tool["description"] = json!(
            "Search the bundled multi-vendor GGUF catalog, filtered by current host RAM. Catalog only: no live HuggingFace search or cached account results."),
        Some("model_recommend") => tool["description"] = json!(
            "Recommend bundled multi-vendor models using current host RAM. Catalog only; unknown GPU memory is not treated as available capacity."),
        _ => {},
    }
}

#[derive(Debug, Serialize)]
struct GpuSnapshot {
    pci_vendor: String,
    pci_device: String,
    vram_total_mb: Option<u64>,
    vram_available_mb: Option<u64>,
}

#[derive(Debug, Serialize)]
struct HardwareSnapshot {
    ram_total_mb: u64,
    ram_available_mb: u64,
    cpu_cores: u32,
    gpus: Vec<GpuSnapshot>,
    gpu_probe: &'static str,
    recommended_backend: &'static str,
    recommended_max_model_gb: f64,
}

fn read_bounded(path: &Path) -> Result<String, VerificationError> {
    let input = std::fs::File::open(path).map_err(|_| VerificationError::Unavailable)?;
    let mut text = String::new();
    input.take(MAX_KERNEL_TEXT as u64 + 1).read_to_string(&mut text)
        .map_err(|_| VerificationError::Unavailable)?;
    if text.len() > MAX_KERNEL_TEXT { return Err(VerificationError::Invalid); }
    Ok(text)
}

fn parse_memory(text: &str) -> Result<(u64, u64), VerificationError> {
    if text.len() > MAX_KERNEL_TEXT { return Err(VerificationError::Invalid); }
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue; };
        let destination = match key {
            "MemTotal" => &mut total, "MemAvailable" => &mut available,
            _ => continue,
        };
        if destination.is_some() { return Err(VerificationError::Invalid); }
        let mut fields = value.split_whitespace();
        let number: u64 = fields.next().ok_or(VerificationError::Invalid)?.parse()
            .map_err(|_| VerificationError::Invalid)?;
        if fields.next() != Some("kB") || fields.next().is_some() {
            return Err(VerificationError::Invalid);
        }
        *destination = Some(number / 1024);
    }
    let (total, available) = (total.ok_or(VerificationError::Unavailable)?,
        available.ok_or(VerificationError::Unavailable)?);
    if total == 0 || available > total { return Err(VerificationError::Invalid); }
    Ok((total, available))
}

fn pci_id(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = value.strip_prefix("0x")?;
    (digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

#[cfg(target_os = "linux")]
fn hardware() -> Result<HardwareSnapshot, VerificationError> {
    let (ram_total_mb, ram_available_mb) = parse_memory(&read_bounded(Path::new("/proc/meminfo"))?)?;
    let mut gpus = Vec::new();
    // Only fixed kernel-owned sysfs locations. No directory names or paths are
    // derived from tool input, and absent driver counters remain unknown.
    for card in 0..16 {
        let device = format!("/sys/class/drm/card{card}/device");
        let vendor = read_bounded(&Path::new(&device).join("vendor")).ok().and_then(|v| pci_id(&v));
        let model = read_bounded(&Path::new(&device).join("device")).ok().and_then(|v| pci_id(&v));
        if let (Some(pci_vendor), Some(pci_device)) = (vendor, model) {
            let counter = |name: &str| read_bounded(&Path::new(&device).join(name)).ok()
                .and_then(|v| v.trim().parse::<u64>().ok()).map(|bytes| bytes / (1024 * 1024));
            let vram_total_mb = counter("mem_info_vram_total");
            let used = counter("mem_info_vram_used");
            let vram_available_mb = vram_total_mb.zip(used).and_then(|(total, used)| total.checked_sub(used));
            gpus.push(GpuSnapshot { pci_vendor, pci_device, vram_total_mb, vram_available_mb });
        }
    }
    Ok(HardwareSnapshot { ram_total_mb, ram_available_mb,
        cpu_cores: std::thread::available_parallelism().map_err(|_| VerificationError::Unavailable)?
            .get().try_into().map_err(|_| VerificationError::Invalid)?,
        gpus, gpu_probe: "bounded_sysfs_pci_only", recommended_backend: "openai_compat",
        recommended_max_model_gb: (ram_available_mb as f64 * 0.7) / 1024.0 })
}

#[cfg(not(target_os = "linux"))]
fn hardware() -> Result<HardwareSnapshot, VerificationError> { Err(VerificationError::Unavailable) }

fn query<'a>(name: &str, args: &'a Value) -> Result<&'a str, VerificationError> {
    let object = args.as_object().ok_or(VerificationError::Invalid)?;
    if object.keys().any(|key| !matches!((name, key.as_str()),
        ("model_search", "query" | "connector_account") | ("model_recommend", "connector_account"))) {
        return Err(VerificationError::Invalid);
    }
    if name != "model_search" { return Ok(""); }
    let query = match args.get("query") {
        Some(value) => value.as_str().ok_or(VerificationError::Invalid)?,
        None => "chat gguf",
    };
    if query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
        return Err(VerificationError::Invalid);
    }
    Ok(query.trim())
}

fn catalog(query: &str, ram_mb: u64) -> Vec<duduclaw_inference::model_registry::RegistryEntry> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    duduclaw_inference::model_registry::curated::builtin_registry().into_iter()
        .filter(|entry| entry.min_ram_mb <= ram_mb)
        .filter(|entry| {
            let searchable = format!("{} {} {} {} {} {} {}", entry.name, entry.repo,
                entry.filename, entry.params, entry.description, entry.tags.join(" "),
                entry.languages.join(" ")).to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        }).take(15).collect()
}

fn authorized(context: &RequestContext, name: &str) -> bool {
    // Catalog-only mode still narrows the original resource policy. It does
    // not turn an absent/revoked model connector into an implicit account.
    supports(name) && tool_policy::eligible(context, name)
        && duduclaw_mcp_boundary::authority::now_seconds().is_ok_and(|now|
            now >= context.record().not_before && now < context.record().expires_at)
}

pub(crate) async fn handle(name: &str, args: &Value, home: Option<&Path>) -> Value {
    let result = async {
        let call = crate::mcp_isolation::current().ok_or(VerificationError::Invalid)?;
        if !authorized(&call.context, name) || home.is_some_and(|home| home != call.home) {
            return Err(VerificationError::BindingMismatch);
        }
        let query = query(name, args)?.to_owned();
        let resolver = call.record_resolver.clone().ok_or(VerificationError::Unavailable)?;
        // Preserve live connector/member binding checks even though catalog
        // reads acquire no credential and make no outbound request.
        let before = tool_policy::authorize_async(&call.context, name, args, resolver.clone()).await?;
        let hw = duduclaw_mcp_boundary::authority::verify_blocking(hardware).await?;
        let after = tool_policy::authorize_async(&call.context, name, args, resolver).await?;
        if before.resources() != after.resources() || !authorized(&call.context, name) {
            return Err(VerificationError::BindingMismatch);
        }
        let payload = if name == "hardware_info" { json!(hw) } else {
            json!({"source":"bundled_catalog", "live_search":false,
                "query":query, "hardware":hw, "models":catalog(&query, hw.ram_available_mb),
                "notice":"僅查詢內建模型目錄，未查詢 HuggingFace 即時資料；硬體資料來自主機的唯讀快照。"})
        };
        Ok(crate::mcp::tool_text(&payload.to_string()))
    }.await;
    result.unwrap_or_else(|_: VerificationError|
        crate::mcp::tool_error("無法確認主機模型查詢的授權或硬體資料，已拒絕這次操作。"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_inference_memory_fields_are_exact_and_bounded() {
        assert_eq!(parse_memory("MemTotal: 8192 kB\nMemAvailable: 4096 kB\n").unwrap(), (8, 4));
        for text in ["MemTotalExtra: 8192 kB\nMemAvailable: 4096 kB\n",
            "MemTotal: 8192 kB\nMemTotal: 8192 kB\nMemAvailable: 4096 kB\n",
            "MemTotal: 8192 bytes\nMemAvailable: 4096 kB\n",
            "MemTotal: 8192 kB trailing\nMemAvailable: 4096 kB\n",
            "MemTotal: 4096 kB\nMemAvailable: 8192 kB\n"] {
            assert!(parse_memory(text).is_err());
        }
        assert!(parse_memory(&"x".repeat(MAX_KERNEL_TEXT + 1)).is_err());
    }

    #[test]
    fn host_inference_catalog_filters_query_and_ram() {
        assert!(catalog("qwen", 0).is_empty());
        let entries = catalog("qwen", u64::MAX);
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|entry| entry.name.to_lowercase().contains("qwen")
            || entry.repo.to_lowercase().contains("qwen")));
        assert!(catalog("no-such-model-19f739", u64::MAX).is_empty());
        assert!(catalog("", u64::MAX).len() <= 15);
    }

    #[test]
    fn host_inference_does_not_accept_probe_or_credential_overrides() {
        for key in ["path", "command", "args", "api_key", "HF_TOKEN", "endpoint", "cache", "root"] {
            let mut input = json!({"query":"qwen"}); input[key] = json!("forged");
            assert!(query("model_search", &input).is_err(), "{key}");
        }
        assert!(query("model_search", &json!({"query":1})).is_err());
        assert!(query("model_search", &json!({"query":"x".repeat(MAX_QUERY_BYTES + 1)})).is_err());
        for value in ["0x10de", "0x1002", "0x8086"] { assert!(pci_id(value).is_some()); }
        for value in ["10de", "0x10de;evil", "0x10dez"] { assert!(pci_id(value).is_none()); }
    }

    #[tokio::test]
    async fn host_inference_requires_authenticated_gateway_context() {
        for name in ["hardware_info", "model_search", "model_recommend"] {
            assert_eq!(handle(name, &json!({}), None).await["isError"], true);
        }
    }

    #[test]
    fn host_inference_schema_states_catalog_and_unknown_hardware_limits() {
        for name in ["model_search", "model_recommend"] {
            let mut tool = json!({"name":name}); describe(&mut tool);
            assert!(tool["description"].as_str().unwrap().contains("Catalog only"));
        }
        let mut hardware = json!({"name":"hardware_info"}); describe(&mut hardware);
        assert!(hardware["description"].as_str().unwrap().contains("unknown"));
        let mut other = json!({"name":"model_download", "description":"unchanged"}); describe(&mut other);
        assert_eq!(other["description"], "unchanged");
    }
}
