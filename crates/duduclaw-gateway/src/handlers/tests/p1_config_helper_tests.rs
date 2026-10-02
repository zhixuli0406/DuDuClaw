//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

// ── P1 dashboard-config helper tests (RT / EVO / CT / INF) ────────────────────
use super::*;

// ── RT: runtime provider enum validation ──

#[test]
fn runtime_valid_provider_and_fallback_written() {
    let mut t = toml::Table::new();
    let params = json!({ "runtime": {
        "provider": "codex",
        "fallback": "claude",
    }});
    let changes = apply_runtime_to_table(&mut t, &params).unwrap();
    assert_eq!(changes.len(), 2);
    let rt = t.get("runtime").unwrap().as_table().unwrap();
    assert_eq!(rt.get("provider").unwrap().as_str(), Some("codex"));
    assert_eq!(rt.get("fallback").unwrap().as_str(), Some("claude"));
}

#[test]
fn runtime_unknown_provider_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "runtime": { "provider": "gpt4" } });
    let err = apply_runtime_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("provider"), "got: {err}");
}

#[test]
fn runtime_unknown_fallback_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "runtime": { "fallback": "bogus" } });
    let err = apply_runtime_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("fallback"), "got: {err}");
}

#[test]
fn runtime_empty_fallback_clears() {
    let mut t = toml::Table::new();
    t.insert(
        "runtime".into(),
        toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("fallback".into(), toml::Value::String("claude".into()));
            m
        }),
    );
    let params = json!({ "runtime": { "fallback": "" } });
    let changes = apply_runtime_to_table(&mut t, &params).unwrap();
    assert!(changes.iter().any(|c| c.contains("cleared")));
    let rt = t.get("runtime").unwrap().as_table().unwrap();
    assert!(rt.get("fallback").is_none());
}

#[test]
fn runtime_absent_object_is_noop() {
    let mut t = toml::Table::new();
    let changes = apply_runtime_to_table(&mut t, &json!({})).unwrap();
    assert!(changes.is_empty());
    assert!(t.get("runtime").is_none());
}

// ── EVO: range validation + external_factors ──

#[test]
fn evolution_advanced_writes_external_factors_and_scalars() {
    let mut t = toml::Table::new();
    let params = json!({ "evolution_advanced": {
        "external_factors": { "user_feedback": true, "peer_signals": false },
        "skill_synthesis_enabled": true,
        "skill_synthesis_threshold": 3,
        "skill_synthesis_cooldown_hours": 12,
        "skill_trial_ttl": 5,
        // H3 regression: a removed knob must be ignored, not written back.
        "curiosity_max_daily": 5,
    }});
    let changes = apply_evolution_advanced_to_table(&mut t, &params).unwrap();
    assert!(!changes.is_empty());
    let evo = t.get("evolution").unwrap().as_table().unwrap();
    let ef = evo.get("external_factors").unwrap().as_table().unwrap();
    assert_eq!(ef.get("user_feedback").unwrap().as_bool(), Some(true));
    assert_eq!(ef.get("peer_signals").unwrap().as_bool(), Some(false));
    assert_eq!(
        evo.get("skill_synthesis_enabled").unwrap().as_bool(),
        Some(true)
    );
    // skill_synthesis_threshold is a u32 gap-count, not a unit threshold —
    // it must serialize as a TOML integer (see apply_evolution_advanced_to_table).
    assert_eq!(
        evo.get("skill_synthesis_threshold").unwrap().as_integer(),
        Some(3)
    );
    assert_eq!(
        evo.get("skill_synthesis_cooldown_hours")
            .unwrap()
            .as_integer(),
        Some(12)
    );
    assert_eq!(evo.get("skill_trial_ttl").unwrap().as_integer(), Some(5));
    // H3 (2026-09-29): the write-only knobs are gone from the form. A
    // client that still sends one must not get it persisted — otherwise
    // the "setting that never takes effect" is back.
    assert!(
        evo.get("curiosity_max_daily").is_none(),
        "a removed evolution knob must not be written into agent.toml"
    );
}

#[test]
fn evolution_threshold_out_of_range_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "evolution_advanced": { "skill_graduation_min_lift": 1.5 } });
    let err = apply_evolution_advanced_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("0.0-1.0"), "got: {err}");
}

#[test]
fn evolution_min_lift_negative_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "evolution_advanced": { "skill_graduation_min_lift": -0.1 } });
    let err = apply_evolution_advanced_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("0.0-1.0"), "got: {err}");
}

#[test]
fn evolution_absent_object_is_noop() {
    let mut t = toml::Table::new();
    let changes = apply_evolution_advanced_to_table(&mut t, &json!({})).unwrap();
    assert!(changes.is_empty());
}

// ── CT: mount parsing ──

#[test]
fn container_advanced_mounts_written() {
    let mut t = toml::Table::new();
    let params = json!({ "container_advanced": {
        "additional_mounts": [
            { "host": "~/projects", "container": "/projects", "readonly": false },
            { "host": "~/Documents", "container": "/docs", "readonly": true },
        ],
        "cmd": ["bash", "-c", "echo hi"],
        "env": [ { "key": "FOO", "value": "bar" }, ["BAZ", "qux"] ],
    }});
    let changes = apply_container_advanced_to_table(&mut t, &params).unwrap();
    assert!(!changes.is_empty());
    let ct = t.get("container").unwrap().as_table().unwrap();
    let mounts = ct.get("additional_mounts").unwrap().as_array().unwrap();
    assert_eq!(mounts.len(), 2);
    let m0 = mounts[0].as_table().unwrap();
    assert_eq!(m0.get("host").unwrap().as_str(), Some("~/projects"));
    assert_eq!(m0.get("readonly").unwrap().as_bool(), Some(false));
    let env = ct.get("env").unwrap().as_array().unwrap();
    assert_eq!(env.len(), 2);
    let e0 = env[0].as_array().unwrap();
    assert_eq!(e0[0].as_str(), Some("FOO"));
    assert_eq!(e0[1].as_str(), Some("bar"));
    let e1 = env[1].as_array().unwrap();
    assert_eq!(e1[0].as_str(), Some("BAZ"));
}

#[test]
fn container_mount_blocked_pattern_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "container_advanced": {
        "additional_mounts": [ { "host": "~/.ssh", "container": "/keys" } ]
    }});
    let err = apply_container_advanced_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("blocked pattern"), "got: {err}");
}

#[test]
fn container_mount_empty_path_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "container_advanced": {
        "additional_mounts": [ { "host": "", "container": "/x" } ]
    }});
    let err = apply_container_advanced_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("host"), "got: {err}");
}

#[test]
fn container_env_bad_arity_rejected() {
    let mut t = toml::Table::new();
    let params = json!({ "container_advanced": { "env": [ ["ONLY_ONE"] ] } });
    let err = apply_container_advanced_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("2 elements"), "got: {err}");
}

// ── INF: router cross-validation + secret masking ──

#[test]
fn inference_router_strong_must_be_less_than_fast() {
    let mut t = toml::Table::new();
    let params = json!({ "router": { "fast_threshold": 0.5, "strong_threshold": 0.6 } });
    let err = apply_inference_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("must be <"), "got: {err}");
}

#[test]
fn inference_router_valid_thresholds_pass() {
    let mut t = toml::Table::new();
    let params = json!({ "router": {
        "enabled": true,
        "fast_threshold": 0.7,
        "strong_threshold": 0.35,
        "cloud_keywords": ["refactor"],
    }});
    let changes = apply_inference_to_table(&mut t, &params).unwrap();
    assert!(!changes.is_empty());
    let r = t.get("router").unwrap().as_table().unwrap();
    assert_eq!(r.get("fast_threshold").unwrap().as_float(), Some(0.7));
    assert_eq!(r.get("strong_threshold").unwrap().as_float(), Some(0.35));
}

#[test]
fn inference_router_uses_existing_fast_for_cross_check() {
    // Existing fast=0.7; incoming strong=0.8 only → must still be rejected.
    let mut t = toml::Table::new();
    t.insert(
        "router".into(),
        toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("fast_threshold".into(), toml::Value::Float(0.7));
            m
        }),
    );
    let params = json!({ "router": { "strong_threshold": 0.8 } });
    let err = apply_inference_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("must be <"), "got: {err}");
}

#[test]
fn inference_generation_temperature_range_enforced() {
    let mut t = toml::Table::new();
    let params = json!({ "generation": { "temperature": 3.0 } });
    let err = apply_inference_to_table(&mut t, &params).unwrap_err();
    assert!(err.contains("temperature"), "got: {err}");
}

#[test]
fn inference_root_and_passthrough_sections_written() {
    let mut t = toml::Table::new();
    let params = json!({
        "enabled": true,
        "backend": "openai_compat",
        "max_memory_mb": 8192,
        "llamafile": { "auto_start": true, "port": 8080 },
        "embedding": { "enabled": false, "model": "bge-small-zh" },
    });
    let changes = apply_inference_to_table(&mut t, &params).unwrap();
    assert!(!changes.is_empty());
    assert_eq!(t.get("enabled").unwrap().as_bool(), Some(true));
    assert_eq!(t.get("max_memory_mb").unwrap().as_integer(), Some(8192));
    let lf = t.get("llamafile").unwrap().as_table().unwrap();
    assert_eq!(lf.get("port").unwrap().as_integer(), Some(8080));
}

#[test]
fn inference_backend_refuses_removed_values_on_write() {
    for removed in ["llama_cpp", "mistral_rs", "mlx", "whatever"] {
        let mut t = toml::Table::new();
        t.insert("enabled".into(), toml::Value::Boolean(false));
        let params = json!({ "backend": removed, "enabled": true });
        let err = apply_inference_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("openai_compat"), "message must name the supported value: {err}");
        // Refused before anything was written.
        assert_eq!(t.get("enabled").unwrap().as_bool(), Some(false));
        assert!(t.get("backend").is_none());
    }
    let mut t = toml::Table::new();
    assert!(apply_inference_to_table(&mut t, &json!({ "backend": 3 })).is_err());
}

#[test]
fn inference_backend_supported_empty_and_stored_echo() {
    // Supported value is written.
    let mut t = toml::Table::new();
    let changes = apply_inference_to_table(&mut t, &json!({ "backend": "openai_compat" })).unwrap();
    assert_eq!(t.get("backend").unwrap().as_str(), Some("openai_compat"));
    assert!(changes.iter().any(|c| c.contains("inference.backend")));

    // A stored removed value still loads and can be echoed back unchanged
    // (the dashboard re-sends the loaded value on every save)...
    let mut t: toml::Table = toml::from_str("backend = \"llama_cpp\"\nenabled = false\n").unwrap();
    let changes =
        apply_inference_to_table(&mut t, &json!({ "backend": "llama_cpp", "enabled": true })).unwrap();
    assert_eq!(t.get("backend").unwrap().as_str(), Some("llama_cpp"));
    assert_eq!(t.get("enabled").unwrap().as_bool(), Some(true));
    assert!(!changes.iter().any(|c| c.contains("inference.backend")));
    // ...but not switched to the other removed value.
    assert!(apply_inference_to_table(&mut t, &json!({ "backend": "mistral_rs" })).is_err());
    // ...and the user can move off it, to the supported value or to auto.
    apply_inference_to_table(&mut t, &json!({ "backend": "openai_compat" })).unwrap();
    assert_eq!(t.get("backend").unwrap().as_str(), Some("openai_compat"));
    let changes = apply_inference_to_table(&mut t, &json!({ "backend": "" })).unwrap();
    assert!(t.get("backend").is_none(), "empty clears the key");
    assert!(changes.iter().any(|c| c.contains("inference.backend")));
    // Absent leaves it alone.
    let mut t: toml::Table = toml::from_str("backend = \"openai_compat\"").unwrap();
    apply_inference_to_table(&mut t, &json!({ "enabled": true })).unwrap();
    assert_eq!(t.get("backend").unwrap().as_str(), Some("openai_compat"));
}

#[test]
fn inference_response_masks_api_key_cleartext() {
    let mut t = toml::Table::new();
    t.insert(
        "openai_compat".into(),
        toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("base_url".into(), toml::Value::String("http://x/v1".into()));
            m.insert(
                "api_key".into(),
                toml::Value::String("sk-supersecret".into()),
            );
            m
        }),
    );
    let resp = inference_table_to_response(&t);
    let oc = resp.get("openai_compat").unwrap();
    let serialised = serde_json::to_string(&resp).unwrap();
    assert!(
        !serialised.contains("sk-supersecret"),
        "cleartext leaked: {serialised}"
    );
    assert_eq!(oc.get("api_key").unwrap().as_str(), Some(SECRET_MASK_SET));
    assert_eq!(oc.get("api_key_set").unwrap().as_bool(), Some(true));
    assert!(oc.get("api_key_enc").is_none());
}

#[test]
fn inference_response_masks_encrypted_key_too() {
    let mut t = toml::Table::new();
    t.insert(
        "openai_compat".into(),
        toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert(
                "api_key_enc".into(),
                toml::Value::String("ENCBLOB==".into()),
            );
            m
        }),
    );
    let resp = inference_table_to_response(&t);
    let serialised = serde_json::to_string(&resp).unwrap();
    assert!(!serialised.contains("ENCBLOB"), "enc leaked: {serialised}");
    let oc = resp.get("openai_compat").unwrap();
    assert_eq!(oc.get("api_key_set").unwrap().as_bool(), Some(true));
}

#[test]
fn inference_response_no_secret_reports_unset() {
    let mut t = toml::Table::new();
    t.insert(
        "openai_compat".into(),
        toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("base_url".into(), toml::Value::String("http://x/v1".into()));
            m
        }),
    );
    let resp = inference_table_to_response(&t);
    let oc = resp.get("openai_compat").unwrap();
    assert_eq!(oc.get("api_key_set").unwrap().as_bool(), Some(false));
    assert_eq!(oc.get("api_key").unwrap().as_str(), Some(""));
}
