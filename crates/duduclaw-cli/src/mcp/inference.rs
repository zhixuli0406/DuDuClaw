use super::*;

pub(crate) async fn handle_inference_status(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let available = engine.is_available().await;
    let hw = engine.hardware_info().await;
    let models = engine.list_models().await;
    let loaded_count = models.iter().filter(|m| m.is_loaded).count();

    let mut status = format!(
        "Inference Engine Status:\n  Enabled: {}\n  Available: {}\n  Models: {} available, {} loaded",
        engine.config().enabled,
        available,
        models.len(),
        loaded_count
    );

    if let Some(ref hw) = hw {
        status.push_str(&format!(
            "\n  GPU: {} ({})\n  RAM: {}MB / {}MB\n  Recommended backend: {}",
            hw.gpu_name,
            format!("{:?}", hw.gpu_type),
            hw.ram_available_mb,
            hw.ram_total_mb,
            hw.recommended_backend
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

pub(crate) async fn handle_model_list(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let models = engine.list_models().await;

    if models.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No models found in ~/.duduclaw/models/\n\nTo get started:\n1. Download a GGUF model (e.g., from huggingface.co)\n2. Place it in ~/.duduclaw/models/\n3. Run model_list again"}]
        });
    }

    let mut text = format!("Available models ({}):\n", models.len());

    text.push_str("\n  (KV cache estimates are approximate lower bounds for typical GQA models)");

    for m in &models {
        let loaded = if m.is_loaded { " [LOADED]" } else { "" };
        let size_mb = m.file_size_bytes / (1024 * 1024);
        let total_mb = m.estimated_memory_mb + m.kv_cache_mb;

        let kv_info = if m.kv_cache_mb == 0 {
            format!("total ~{}MB", m.estimated_memory_mb)
        } else {
            format!("KV ~{}MB, total ~{}MB", m.kv_cache_mb, total_mb)
        };

        text.push_str(&format!(
            "\n  {} ({} {} {}) — {}MB weights, {}{loaded}",
            m.id, m.architecture, m.parameter_count, m.quantization, size_mb, kv_info,
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

pub(crate) async fn handle_model_load(params: &Value, home_dir: &Path) -> Value {
    let model_id = params
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if model_id.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "model_id is required"}]
        });
    }

    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to init inference engine: {e}")}]
        });
    }

    match engine.load_model(model_id).await {
        Ok(info) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Model loaded: {} ({} {} {})\nEstimated memory: {}MB\nContext length: {}",
                info.id, info.architecture, info.parameter_count, info.quantization,
                info.estimated_memory_mb, info.context_length
            )}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to load model: {e}")}]
        }),
    }
}

pub(crate) async fn handle_model_unload(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to init inference engine: {e}")}]
        });
    }

    match engine.unload_model().await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "Model unloaded successfully. Memory freed."}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to unload: {e}")}]
        }),
    }
}

pub(crate) async fn handle_hardware_info() -> Value {
    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let text = format!(
        "Hardware Detection Results:\n\
         \n  GPU: {} ({:?})\
         \n  VRAM: {}MB total, {}MB available\
         \n  RAM: {}MB total, {}MB available\
         \n  CPU cores: {}\
         \n  Recommended backend: {}\
         \n  Recommended max model: {:.1}GB",
        hw.gpu_name,
        hw.gpu_type,
        hw.vram_total_mb,
        hw.vram_available_mb,
        hw.ram_total_mb,
        hw.ram_available_mb,
        hw.cpu_cores,
        hw.recommended_backend,
        hw.recommended_max_model_gb,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

pub(crate) async fn handle_route_query(params: &Value, home_dir: &Path) -> Value {
    let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let system_prompt = params
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if prompt.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "prompt is required"}]
        });
    }

    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let decision = engine.route(system_prompt, prompt);

    let text = format!(
        "Routing Decision:\n\
         \n  Tier: {}\
         \n  Confidence: {:.2}\
         \n  Model: {}\
         \n  Reason: {}\
         \n  Router enabled: {}",
        decision.tier,
        decision.confidence,
        decision.model_id.as_deref().unwrap_or("(cloud api)"),
        decision.reason,
        engine.router_enabled(),
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

pub(crate) async fn handle_inference_mode(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let mode = engine.current_mode().await;
    let status = engine.manager().status().await;

    let text = format!(
        "Inference Manager Status:\n\
         \n  Current mode: {}\
         \n  llamafile: {}",
        mode,
        if status.llamafile_available {
            "running"
        } else {
            "stopped"
        },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

pub(crate) async fn handle_llamafile_start(params: &Value, home_dir: &Path) -> Value {
    let _file = params.get("file").and_then(|v| v.as_str());
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;

    match engine.manager().start_llamafile().await {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": "llamafile server started successfully"}]
        }),
        Err(e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Failed to start llamafile: {e}")}]
        }),
    }
}

pub(crate) async fn handle_llamafile_stop(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    engine.manager().stop_llamafile().await;
    serde_json::json!({
        "content": [{"type": "text", "text": "llamafile server stopped"}]
    })
}

pub(crate) async fn handle_llamafile_list(home_dir: &Path) -> Value {
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    let files = engine.manager().list_llamafiles().await;

    if files.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No llamafiles found in ~/.duduclaw/llamafiles/\n\nTo get started:\n1. Download a .llamafile from huggingface.co or github.com/Mozilla-Ocho/llamafile\n2. Place it in ~/.duduclaw/llamafiles/\n3. Run llamafile_list again"}]
        });
    }

    let text = format!(
        "Available llamafiles ({}):\n{}",
        files.len(),
        files
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

// ── Model registry handlers ─────────────────────────────────

pub(crate) async fn handle_model_search(params: &Value, home_dir: &Path) -> Value {
    let query = params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("chat gguf");

    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let results = duduclaw_inference::model_registry::hf_api::search_models(
        query,
        hw.ram_available_mb,
        home_dir,
    )
    .await;

    // Also include curated
    let curated = duduclaw_inference::model_registry::curated::builtin_registry();
    let curated_filtered = duduclaw_inference::model_registry::curated::filter_by_hardware(
        &curated,
        hw.ram_available_mb,
    );

    let mut all = curated_filtered;
    for r in &results {
        if !all
            .iter()
            .any(|a| a.repo == r.repo && a.filename == r.filename)
        {
            all.push(r.clone());
        }
    }

    if all.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("No models found for query: '{query}' (RAM: {} MB)", hw.ram_available_mb)}]
        });
    }

    let mut text = format!(
        "Models for '{}' (RAM: {} MB):\n",
        query, hw.ram_available_mb
    );
    for (i, e) in all.iter().enumerate().take(15) {
        let tier = match e.tier {
            duduclaw_inference::model_registry::ModelTier::Recommended => "[推薦]",
            duduclaw_inference::model_registry::ModelTier::Community => "[社群]",
        };
        text.push_str(&format!(
            "\n  {}. {} {} ({}, {}) — {}\n     repo: {} file: {}",
            i + 1,
            tier,
            e.name,
            e.params,
            e.size_display(),
            e.description,
            e.repo,
            e.filename
        ));
    }

    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

pub(crate) async fn handle_model_download(params: &Value, home_dir: &Path) -> Value {
    let repo = params.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let filename = params
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if repo.is_empty() || filename.is_empty() {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": "repo and filename are required"}]
        });
    }

    // C-2: validate repo format (owner/name, safe characters only)
    if let Err(e) = duduclaw_inference::model_registry::downloader::validate_repo(repo) {
        return serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("{e}")}]
        });
    }

    let models_dir = home_dir.join("models");
    let entry = duduclaw_inference::model_registry::RegistryEntry {
        name: String::new(),
        repo: repo.to_string(),
        filename: filename.to_string(),
        size_bytes: 0,
        quantization: String::new(),
        params: String::new(),
        languages: vec![],
        tags: vec![],
        min_ram_mb: 0,
        description: String::new(),
        tier: duduclaw_inference::model_registry::ModelTier::Community,
        downloads: 0,
        shards: vec![],
    };

    match duduclaw_inference::model_registry::downloader::download_model(
        &entry.download_url(),
        &entry.mirror_url(),
        &models_dir,
        filename,
        None,
    )
    .await
    {
        Ok(path) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Downloaded to: {}", path.display())}]
        }),
        Err(_e) => serde_json::json!({
            "isError": true,
            "content": [{"type": "text", "text": format!("Download failed. Check logs for details.\nManual URL: {}", entry.download_url())}]
        }),
    }
}

pub(crate) async fn handle_model_recommend(_home_dir: &Path) -> Value {
    let hw = duduclaw_inference::hardware::detect_hardware().await;
    let curated = duduclaw_inference::model_registry::curated::builtin_registry();
    let filtered = duduclaw_inference::model_registry::curated::filter_by_hardware(
        &curated,
        hw.ram_available_mb,
    );

    let mut text = format!(
        "Hardware: {} ({:?})\nRAM: {} MB available / {} MB total\nRecommended max model: {:.1} GB\n\nRecommended models:\n",
        hw.gpu_name, hw.gpu_type, hw.ram_available_mb, hw.ram_total_mb, hw.recommended_max_model_gb
    );

    if filtered.is_empty() {
        text.push_str("\n  No models fit in available RAM.");
    } else {
        for (i, e) in filtered.iter().enumerate() {
            text.push_str(&format!(
                "\n  {}. {} ({}, {}) — {}\n     repo: {} file: {}",
                i + 1,
                e.name,
                e.params,
                e.size_display(),
                e.description,
                e.repo,
                e.filename
            ));
        }
    }

    serde_json::json!({"content": [{"type": "text", "text": text}]})
}

// ── Cost telemetry handlers ─────────────────────────────────
