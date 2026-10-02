use super::contracts::{PolicyDegraded, PolicySource};
use super::policy::BASELINE_POLICY_ID;
use super::policy_runner::{ManagedPolicySource, PythonPolicyRuntime, BASELINE_SOURCE, evaluate_candidate};
use super::tree::WorldTree;

fn runtime() -> Option<PythonPolicyRuntime> {
    match PythonPolicyRuntime::experimental_native_for_tests() {
        Ok(runtime) => Some(runtime),
        Err(reason) => {
            #[cfg(target_os = "macos")]
            panic!("policy runtime must be exercised on macOS: {reason}");
            #[cfg(not(target_os = "macos"))]
            { eprintln!("policy runtime unavailable: {reason}"); None }
        }
    }
}
fn world() -> WorldTree {
    WorldTree::load_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/discovery/full_grid")).unwrap()
}
fn policy(body: &str) -> String {
    format!("class OptimalPolicy:\n    def __init__(self, config):\n        self.beta = config.get('beta', 0.6)\n    def plan_grid(self, context):\n        return {{'branch_count': context['hard_max_branch_count'], 'refine_count': context['hard_max_refine_count'], 'reason': 't'}}\n    def solve(self, question):\n{body}\n")
}

#[test]
fn no_isolation_or_python_explicitly_degrades_to_builtin() {
    for reason in [PolicyDegraded::NoIsolation, PolicyDegraded::NoPython] {
        let source = ManagedPolicySource::new(Err(reason.clone()));
        assert_eq!(source.degraded(), Some(reason));
        assert_eq!(source.current().policy_id, BASELINE_POLICY_ID);
        assert_eq!(source.instantiate(0.5).unwrap().id(), BASELINE_POLICY_ID);
        assert!(source.install(BASELINE_SOURCE).is_err());
    }
}

#[test]
fn static_gate_rejects_adversarial_samples_with_specific_reasons() {
    let Some(runtime) = runtime() else { return };
    for (source, reason) in [
        ("import os\nclass OptimalPolicy: pass", "ast:forbidden_import:os"),
        ("import socket\nclass OptimalPolicy: pass", "ast:forbidden_import:socket"),
        ("import subprocess\nclass OptimalPolicy: pass", "ast:forbidden_import:subprocess"),
        ("from pathlib import Path\nclass OptimalPolicy: pass", "ast:forbidden_import:pathlib"),
        ("import random\nclass OptimalPolicy: pass", "ast:forbidden_import:random"),
        ("from . import x\nclass OptimalPolicy: pass", "ast:relative_import"),
        ("from typing import sys as s\nclass OptimalPolicy: pass", "ast:forbidden_name:sys"),
        ("import collections\nclass OptimalPolicy:\n    x = collections._sys", "ast:private_attribute:_sys"),
        ("import typing\nclass OptimalPolicy:\n    x = typing.sys", "ast:forbidden_attribute:sys"),
        ("from dataclasses import inspect as i\nclass OptimalPolicy: pass", "ast:forbidden_name:inspect"),
        ("class OptimalPolicy:\n    x = (n for n in []).gi_frame", "ast:frame_attribute:gi_frame"),
        ("class OptimalPolicy:\n    def helper(self):\n        return self._sys", "ast:private_attribute:_sys"),
        ("import typing\nclass OptimalPolicy:\n    x: \"(__import__('builtins').print('X'), int)[1]\"\ntyping.get_type_hints(OptimalPolicy)", "ast:forbidden_attribute:get_type_hints"),
        ("import typing\nclass OptimalPolicy: pass\ntyping.evaluate_forward_ref(typing.get_args(typing.List[\"__import__('builtins').print('X')\"])[0])", "ast:forbidden_attribute:evaluate_forward_ref"),
        ("class OptimalPolicy:\n    x = open('/tmp/secret')", "ast:forbidden_name:open"),
        ("class OptimalPolicy:\n    x = eval('1')", "ast:forbidden_name:eval"),
        ("class OptimalPolicy:\n    x = exec('1')", "ast:forbidden_name:exec"),
        ("class OptimalPolicy:\n    x = compile('1', 'x', 'exec')", "ast:forbidden_name:compile"),
        ("class OptimalPolicy:\n    x = __import__('os')", "ast:forbidden_name:__import__"),
        ("class OptimalPolicy:\n    x = getattr(1, 'x')", "ast:forbidden_name:getattr"),
        ("class OptimalPolicy:\n    x = __builtins__", "ast:forbidden_name:__builtins__"),
        ("class OptimalPolicy:\n    x = ().__class__", "ast:dunder_attribute:__class__"),
        ("x = 1", "ast:missing_OptimalPolicy"),
    ] {
        assert_eq!(runtime.check_source(source).unwrap_err().to_string(),
            format!("policy rejected: {reason}"), "{source}");
    }
    runtime.check_source(BASELINE_SOURCE).unwrap();
}

#[test]
fn missing_container_image_explicitly_degrades_without_native_fallback() {
    assert!(matches!(PythonPolicyRuntime::detect_container_for_tests("dudu-policy-missing-image-for-test:never"),
        Err(PolicyDegraded::NoIsolation)));
}

#[test]
#[ignore = "requires the official python:3.12-alpine image and a running Docker daemon"]
fn production_container_smoke_uses_resource_caps_and_prefix_protocol() {
    let runtime = PythonPolicyRuntime::detect().expect("verified resource-capped container");
    runtime.check_source(BASELINE_SOURCE).unwrap();
    let source = ManagedPolicySource::new(Ok(runtime));
    source.install(BASELINE_SOURCE).unwrap();
    let tree = world();
    let result = super::eval::run_point(&mut *source.instantiate(0.6).unwrap(),
        &tree, &super::eval::ReplayConfig::for_world(&tree), 0.6).unwrap();
    assert!(result.trace.counters.probes > 0);
    assert!(source.degraded().is_none());
}

#[test]
fn python_baseline_matches_rust_and_different_hash_seeds_detect_nondeterminism() {
    let Some(runtime) = runtime() else { return };
    let tree = world();
    let good = evaluate_candidate(&runtime, BASELINE_SOURCE, std::slice::from_ref(&tree));
    assert!(good.valid, "{:?}", good.violation);
    let rust = super::eval::evaluate_world(
        &|cfg| Box::new(super::policy::BaselineParallelRefine::new(cfg)),
        &tree, &super::eval::ReplayConfig::for_world(&tree)).unwrap();
    assert_eq!(good.worlds[0], rust);
    let bad = evaluate_candidate(&runtime,
        &policy("        roots = question.legal_roots()\n        k = 1 + hash('b') % 2\n        question.probe_batch(roots[:k])"), &[tree]);
    assert!(!bad.valid);
    assert!(bad.violation.unwrap().contains("nondeterministic"));
}

#[test]
fn two_illegal_batches_exceptions_and_timeout_invalidate_policy() {
    let Some(runtime) = runtime() else { return };
    for (body, reason) in [
        ("        raise ValueError('boom')", "policy_exception"),
        ("        for i in range(2):\n            try:\n                question.probe_batch([])\n            except Exception:\n                pass", "consecutive illegal batches"),
    ] {
        let result = evaluate_candidate(&runtime, &policy(body), &[world()]);
        assert!(!result.valid);
        assert!(result.violation.unwrap().contains(reason));
    }
    let source = ManagedPolicySource::new(Ok(runtime));
    source.install(&policy("        while True:\n            pass")).unwrap();
    source.set_online_timeout(std::time::Duration::from_millis(150));
    let started = std::time::Instant::now();
    let tree = world();
    let mut replay = super::replay::Replay::new(&tree, 1000);
    let error = source.instantiate(0.6).unwrap().solve(&mut replay).unwrap_err();
    assert!(error.to_string().contains("timeout"));
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert!(source.degraded().is_some(), "invalid version is retired to baseline");
}

#[test]
fn refused_metadata_query_is_advisory_and_does_not_count_as_illegal_batch() {
    let Some(runtime) = runtime() else { return };
    let result = evaluate_candidate(&runtime, &policy(
        "        try:\n            question.meta('r1-b0-a2')\n        except Exception:\n            pass\n        question.probe_batch(question.legal_roots()[:1])"), &[world()]);
    assert!(result.valid, "{:?}", result.violation);
    assert!(result.worlds[0].points.iter().all(|p| p.probes == 1));
}

#[test]
fn confinement_denies_file_reads_writes_network_and_ambient_environment() {
    let Some(runtime) = runtime() else { return };
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("private-canary");
    std::fs::write(&secret, "must not leak").unwrap();
    let target = dir.path().join("write-canary");
    let source = format!("import os,socket,json\ndenied=[]\ntry:\n open({}).read()\nexcept PermissionError:\n denied.append('read')\ntry:\n open({},'w').write('bad')\nexcept PermissionError:\n denied.append('write')\ntry:\n socket.socket().connect(('127.0.0.1',9))\nexcept PermissionError:\n denied.append('network')\nassert 'HOME' not in os.environ and 'ANTHROPIC_API_KEY' not in os.environ\nprint(json.dumps(denied))",
        serde_json::to_string(&secret.to_string_lossy()).unwrap(),
        serde_json::to_string(&target.to_string_lossy()).unwrap());
    let denied: Vec<String> = serde_json::from_str(&runtime.run_test_code(&source).unwrap()).unwrap();
    assert_eq!(denied, ["read", "write", "network"]);
    assert!(!target.exists());
}
