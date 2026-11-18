//! R7d policy generator: source parsing and refusals, profile id derivation,
//! digests of real files, and generated documents through the production checks.
use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const API_WORKER: &str = "\u{7f}ELF TEST api worker bytes";
const FORWARDER: &str = "\u{7f}ELF TEST egress forwarder bytes";
const BRIDGE: &str = "\u{7f}ELF TEST mcp bridge bytes";

struct Fixture { dir: tempfile::TempDir }
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = dir.path().join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        for (name, contents) in [("runtime-api-worker-v1", API_WORKER), ("duduclaw-egress-forwarder", FORWARDER),
            ("duduclaw-mcp-bridge", BRIDGE), ("runtime-other-v1", "\u{7f}ELF TEST other"),
            ("runtime-vendor-codex", "\u{7f}ELF TEST vendor"), ("duduclaw", "\u{7f}ELF not hashed")] {
            fs::write(artifacts.join(name), contents).unwrap();
        }
        fs::write(dir.path().join("x86_64.syscalls"), "# TEST-ONLY list\n0-334   # all classic calls\n424-467\n").unwrap();
        fs::write(dir.path().join("aarch64.syscalls"), "0-293\n424-467\n").unwrap();
        // execveat (322) missing; exit_group (231) present.
        fs::write(dir.path().join("x86_64-no-execveat.syscalls"), "0-321\n").unwrap();
        fs::write(dir.path().join("names.syscalls"), "read\n").unwrap();
        Fixture { dir }
    }
    fn artifacts(&self) -> PathBuf { self.dir.path().join("artifacts") }
    fn seccomp() -> serde_json::Value { json!({"x86_64": "x86_64.syscalls", "aarch64": "aarch64.syscalls"}) }
    /// The shape of the shipped TEST fixture (one OpenAI-compatible egress runtime).
    fn source(&self) -> serde_json::Value {
        json!({
            "schema": SOURCE_SCHEMA,
            "dns_probe_name": "example.com",
            "forwarder_seccomp": Self::seccomp(),
            "runtimes": [{
                "label": "api-worker-test",
                "runtime": {"kind": "open_ai_compat"},
                "executable": "runtime-api-worker-v1",
                "pinned_version": "api-worker-v1-test",
                "fixed_args": "default",
                "resources": {"memory_bytes": 1073741824u64, "cpu_quota_percent": 100, "tasks": 256, "open_files": 4096,
                    "ptys": 1, "disk": {"kind": "loop_filesystem", "bytes": 1073741824u64, "inodes": 65536},
                    "io_read_bytes_per_sec": 52428800, "io_write_bytes_per_sec": 52428800, "io_read_ops_per_sec": 2000,
                    "io_write_ops_per_sec": 2000, "output_buffer_bytes": 65536, "output_total_bytes": 16777216,
                    "memfd_total_bytes": 8388608},
                "timeouts": {"queue_ms": 60000, "startup_ms": 30000, "runtime_ms": 600000, "pty_idle_ms": null,
                    "termination_grace_ms": 10000},
                "network": {"kind": "egress", "profile": "test-egress"},
                "mcp": true, "pty_allowed": false, "queue_class": "background",
                "native": {"seccomp": Self::seccomp(), "tmpfs_bytes": 67108864, "tmpfs_inodes": 4096, "proc_subset_pid": true}
            }]
        })
    }
    fn write_source(&self, source: &serde_json::Value) -> PathBuf {
        let path = self.dir.path().join("source.json");
        fs::write(&path, serde_json::to_vec(source).unwrap()).unwrap();
        path
    }
    fn build(&self, source: &serde_json::Value, arch: Arch) -> Checked<(Generated, SignedPolicy)> {
        build_policy(&self.write_source(source), &self.artifacts(), arch, None)
    }
    fn refusal(&self, source: &serde_json::Value) -> String {
        self.build(source, Arch::X86_64).err().expect("the source must be refused")
    }
}
fn sha(contents: &str) -> Sha256Digest { Sha256Digest(Sha256::digest(contents.as_bytes()).into()) }
fn args(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }

#[test]
fn profile_id_is_the_domain_separated_sha256_of_the_label() {
    let mut expected = b"duduclaw-isolation-profile-v1\0".to_vec();
    expected.extend_from_slice(b"api-worker-test");
    assert_eq!(profile_id("api-worker-test"), ProfileId(Sha256Digest(Sha256::digest(&expected).into())));
    assert_eq!(profile_id("a"), profile_id("a"));
    assert_ne!(profile_id("a"), profile_id("b"));
    // The domain separator: no label can collide with the bare sha256 of another string.
    assert_ne!(profile_id("a").0, Sha256Digest(Sha256::digest(b"a").into()));
    let (longest, too_long) = ("a".repeat(64), "a".repeat(65));
    for good in ["a", "test-egress", "x1-2", longest.as_str()] { assert!(check_label("label", good).is_ok(), "{good}"); }
    for bad in ["", "API", "-a", "a-", "a_b", "a.b", "a b", too_long.as_str()] {
        assert!(check_label("label", bad).err().unwrap().contains("label charset"), "{bad}");
    }
}

#[test]
fn syscall_lists_are_decimal_numbers_and_ranges_with_comments() {
    assert_eq!(parse_syscalls("# c\n\n 3 \n0-2 # read..open\n").unwrap(), vec![0, 1, 2, 3]);
    assert_eq!(parse_syscalls("").unwrap(), Vec::<u32>::new());
    for bad in ["read", "-1", "+5", "0x10", "1-", "5-3", "0-600", "1 2", "4294967296"] {
        assert!(parse_syscalls(bad).is_err(), "{bad}");
    }
    assert!(parse_syscalls("0-300\n301-600\n").err().unwrap().contains("512"));
    // Duplicates and an empty list are refused by the production canonical form.
    let fixture = Fixture::new();
    fs::write(fixture.dir.path().join("dup.syscalls"), "1\n1\n").unwrap();
    fs::write(fixture.dir.path().join("empty.syscalls"), "# nothing\n").unwrap();
    for file in ["dup.syscalls", "empty.syscalls"] {
        let files = PerArch { x86_64: Some(file.into()), aarch64: None };
        let error = seccomp_profile(fixture.dir.path(), &files, Arch::X86_64, "test").err().unwrap();
        assert!(error.contains("RuntimeProfile::canonical_bytes"), "{error}");
    }
}

#[test]
fn digests_come_from_the_real_files_and_the_scanned_manifest() {
    let fixture = Fixture::new();
    let artifacts = fixture.artifacts();
    assert_eq!(file_digest(&artifacts.join("runtime-api-worker-v1")).unwrap(), (sha(API_WORKER), API_WORKER.len() as u64));
    std::os::unix::fs::symlink(artifacts.join("runtime-api-worker-v1"), artifacts.join("runtime-linked-v1")).unwrap();
    assert!(file_digest(&artifacts.join("runtime-linked-v1")).err().unwrap().contains("symbolic links"));
    fs::remove_file(artifacts.join("runtime-linked-v1")).unwrap();
    fs::write(artifacts.join("runtime-isolation-policy-v1"), "{}").unwrap();
    let scanned = scan_artifacts(&artifacts, false).unwrap();
    let names: Vec<_> = scanned.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(names, ["duduclaw-egress-forwarder", "duduclaw-mcp-bridge", "runtime-api-worker-v1", "runtime-other-v1",
        "runtime-vendor-codex"]);
    let forwarder = &scanned[0];
    assert_eq!((forwarder.install_target.clone(), forwarder.sha256, forwarder.mode), (InstallTarget::EgressForwarder, sha(FORWARDER), 0o755));
    assert_eq!(scanned[2].install_target, InstallTarget::RuntimeEntry { registry_id: "api-worker-v1".into() });
    let with_policy = scan_artifacts(&artifacts, true).unwrap();
    let policy = with_policy.iter().find(|a| a.id == "runtime-isolation-policy-v1").unwrap();
    assert_eq!((policy.mode, policy.sha256), (0o444, sha("{}")));
    // Each generation name maps back to exactly its target.
    for name in ["duduclaw", "duduclaw-egress", "runtime-", "runtime-a/b", "x-runtime-a"] { assert!(generation_target(name).is_none(), "{name}"); }
}

#[test]
fn generated_policy_passes_every_production_check() {
    let fixture = Fixture::new();
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let (generated, policy) = fixture.build(&fixture.source(), arch).unwrap();
        // The production readers once more, directly on the bytes.
        let parsed = parse_policy(&generated.bytes).unwrap();
        assert_eq!((parsed.version, parsed.dns_probe_name.as_deref()), (4, Some("example.com")));
        assert_eq!(signed_policy_egress_profiles(&generated.bytes).unwrap(), vec![profile_id("test-egress")]);
        assert_eq!(generated.runtimes, vec![("api-worker-test".to_owned(), profile_id("api-worker-test"))]);
        assert_eq!(generated.egress, vec![("test-egress".to_owned(), profile_id("test-egress"))]);
        let manifest = synthetic_manifest(arch, scan_artifacts(&fixture.artifacts(), false).unwrap()).unwrap();
        assert!(verified_policy(&generated.bytes, &manifest, arch).is_ok());
        let record = &policy.runtimes[0];
        assert_eq!(record.profile, profile_id("api-worker-test"));
        assert_eq!(record.network, NetworkPolicy::Egress(profile_id("test-egress")));
        let ExecutableRecord::Elf { entry, digest } = &record.executable else { panic!("ELF") };
        assert_eq!((entry.as_str(), *digest), ("runtime-api-worker-v1", sha(API_WORKER)));
        let arguments: Vec<&str> = record.fixed_args.iter().map(|arg| arg.as_str()).collect();
        assert_eq!(arguments, duduclaw_mcp_boundary::config::api_worker_arguments());
        assert!(record.mcp && record.mcp_acceptance.is_none() && record.turn_limit.is_none());
        let native = &policy.native[0];
        assert_eq!(native.runtime_digest, native.runtime.digest(arch).unwrap());
        assert!(duduclaw_run::seccomp::runtime(arch, &native.runtime, native.runtime_digest).is_ok());
        assert!(native.vendor_digest.is_none());
        let forwarder = native.forwarder.as_ref().unwrap();
        assert_eq!(forwarder.argv, ["/runtime/duduclaw-egress-forwarder", "forwarder", "18080"]);
        assert_eq!(crate::policy::RUNTIME_PROXY_PORT, 18080);
        assert_eq!((forwarder.entry.as_str(), forwarder.executable_digest), ("duduclaw-egress-forwarder", sha(FORWARDER)));
        assert_eq!(forwarder.seccomp_digest, forwarder.seccomp.digest(arch).unwrap());
        // The same list hashes differently per architecture (audit arch in the canonical form).
        assert_ne!(native.runtime.digest(Arch::X86_64).unwrap(), native.runtime.digest(Arch::Aarch64).unwrap());
        // The policy runs under the derived runtime-environment rule with the forwarder port.
        let env = crate::policy::runtime_environment(&record.network, Some(crate::policy::RUNTIME_PROXY_PORT)).unwrap();
        assert_eq!(env["HTTPS_PROXY"], "http://127.0.0.1:18080");
        // The top-level shape the Python offline validator expects for v4.
        let value: serde_json::Value = serde_json::from_slice(&generated.bytes).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 5);
        for key in ["domain", "version", "runtimes", "native", "dns_probe_name"] { assert!(keys.contains(&key), "{key}"); }
        assert_eq!(value["domain"], "duduclaw-isolation-policy-v4");
        // A generated aarch64 policy does not pass as an x86_64 one (seccomp digests).
        if arch == Arch::Aarch64 {
            let x86 = synthetic_manifest(Arch::X86_64, scan_artifacts(&fixture.artifacts(), false).unwrap()).unwrap();
            assert!(check_policy(&generated.bytes, &x86, Arch::X86_64, false, false).err().unwrap().contains("seccomp::runtime"));
        }
    }
}

#[test]
fn shipped_test_fixture_builds_for_both_architectures() {
    let fixture = Fixture::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/live-test/linux-protected/fixtures/policy-source-test.json");
    for arch in [Arch::X86_64, Arch::Aarch64] {
        let (generated, policy) = build_policy(&source, &fixture.artifacts(), arch, None).unwrap();
        assert_eq!(policy.runtimes.len(), 1);
        assert_eq!(policy.runtimes[0].runtime, RuntimeKind::OpenAiCompat);
        assert!(matches!(policy.runtimes[0].network, NetworkPolicy::Egress(_)));
        assert!(policy.runtimes[0].mcp && policy.runtimes[0].mcp_acceptance.is_none());
        assert_eq!(policy.dns_probe_name.as_deref(), Some("example.com"));
        assert!(summary(&policy, &generated.bytes, arch).contains("ABSENT"));
    }
}

#[test]
fn each_source_rule_violation_is_refused_naming_the_rule() {
    let fixture = Fixture::new();
    let base = fixture.source();
    let with = |change: &dyn Fn(&mut serde_json::Value)| { let mut value = base.clone(); change(&mut value); value };
    let offline = |s: &mut serde_json::Value| { s["runtimes"][0]["network"] = json!({"kind": "offline"}); };
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("unknown field `extra`", with(&|s| s["runtimes"][0]["extra"] = true.into())),
        ("unknown field `turn`", with(&|s| s["turn"] = 1.into())),
        ("schema must be", with(&|s| s["schema"] = "duduclaw-isolation-policy-source-v2".into())),
        ("label charset", with(&|s| s["runtimes"][0]["label"] = "API".into())),
        ("unique runtime labels", with(&|s| { let copy = s["runtimes"][0].clone(); s["runtimes"].as_array_mut().unwrap().push(copy); })),
        ("disjoint", with(&|s| s["runtimes"][0]["network"]["profile"] = "api-worker-test".into())),
        ("runtimes: 1 to 128", with(&|s| s["runtimes"] = json!([]))),
        ("UnitPolicy::new", with(&|s| s["runtimes"][0]["resources"]["open_files"] = 1183.into())),
        ("UnitPolicy::new", with(&|s| s["runtimes"][0]["resources"]["memfd_total_bytes"] = (64u64 * 1024 * 1024 + 1).into())),
        ("UnitPolicy::new", with(&|s| s["runtimes"][0]["resources"]["ptys"] = 0.into())),
        ("UnitPolicy::new", with(&|s| s["runtimes"][0]["timeouts"]["runtime_ms"] = 0.into())),
        ("LaunchPolicy::validate", with(&|s| s["runtimes"][0]["timeouts"]["queue_ms"] = 0.into())),
        ("LaunchPolicy::validate", with(&|s| s["runtimes"][0]["timeouts"]["startup_ms"] = 300_001.into())),
        ("PTY run", with(&|s| s["runtimes"][0]["pty_allowed"] = true.into())),
        ("RootConfig::validate", with(&|s| s["runtimes"][0]["native"]["tmpfs_bytes"] = (1u64 << 31).into())),
        ("RootConfig::validate", with(&|s| s["runtimes"][0]["native"]["tmpfs_inodes"] = 0.into())),
        ("from_verified_publication", with(&|s| s["runtimes"][0]["pinned_version"] = "latest".into())),
        ("from_verified_publication", with(&|s| s["runtimes"][0]["pinned_version"] = "".into())),
        ("runtime-missing-v1", with(&|s| s["runtimes"][0]["executable"] = "runtime-missing-v1".into())),
        ("generation file name", with(&|s| s["runtimes"][0]["executable"] = "../runtime-api-worker-v1".into())),
        ("validate_runtime_artifacts", with(&|s| s["runtimes"][0]["executable"] = "runtime-other-v1".into())),
        ("execveat (322)", with(&|s| s["runtimes"][0]["native"]["seccomp"]["x86_64"] = "x86_64-no-execveat.syscalls".into())),
        ("forwarder seccomp", with(&|s| s["forwarder_seccomp"]["x86_64"] = "x86_64-no-execveat.syscalls".into())),
        ("no seccomp allow-list file for x86_64", with(&|s| s["runtimes"][0]["native"]["seccomp"] = json!({"aarch64": "aarch64.syscalls"}))),
        ("syscall names are not accepted", with(&|s| s["runtimes"][0]["native"]["seccomp"]["x86_64"] = "names.syscalls".into())),
        ("validate_dns_probe_name", with(&|s| s["dns_probe_name"] = "localhost".into())),
        ("validate_dns_probe_name", with(&|s| s["dns_probe_name"] = "Example.com".into())),
        ("S5", with(&|s| { offline(s); s.as_object_mut().unwrap().remove("forwarder_seccomp"); })),
        ("forwarder_seccomp is required", with(&|s| { s.as_object_mut().unwrap().remove("forwarder_seccomp"); })),
        ("forwarder_seccomp is given", with(&|s| offline(s))),
        ("turn_limit", with(&|s| s["runtimes"][0]["turn_limit"] = json!({"min": 1, "max": 200, "default": 50}))),
        ("mcp: false must list", with(&|s| s["runtimes"][0]["mcp"] = false.into())),
        ("fixed_args must be", with(&|s| s["runtimes"][0]["fixed_args"] = "auto".into())),
        ("`model` only selects", with(&|s| s["runtimes"][0]["model"] = "gpt".into())),
        ("validate_runtime_artifacts", with(&|s| s["runtimes"][0]["runtime"] = json!({"kind": "codex"}))),
        ("validate_launchers", with(&|s| s["runtimes"][0]["native"]["launcher"] = json!({
            "reserve": {"memory_bytes": 1, "tasks": 1, "cpu_quota_percent": 1}, "helpers": [], "helper_tmpfs_bytes": 1}))),
    ];
    for (rule, source) in cases {
        let error = fixture.refusal(&source);
        assert!(error.contains(rule), "expected {rule:?} in {error:?}");
    }
    // Explicit fixed_args that are not the gateway shape still publish (e.g. a
    // helper); "default" for an offline non-MCP runtime is refused above.
    let explicit = with(&|s| s["runtimes"][0]["fixed_args"] = json!(["--duduclaw-protocol", "openai-compat-v1", "--mcp-config", "/config/mcp.json"]));
    assert!(fixture.build(&explicit, Arch::X86_64).is_ok());
}

#[test]
fn build_writes_a_read_only_file_once_and_verify_accepts_it() {
    let fixture = Fixture::new();
    let source = fixture.write_source(&fixture.source());
    let artifacts = fixture.artifacts();
    let out = fixture.dir.path().join("policy.json");
    let build = args(&["build", "--source", source.to_str().unwrap(), "--artifacts-dir", artifacts.to_str().unwrap(),
        "--arch", "x86_64", "--out", out.to_str().unwrap()]);
    let text = run(&build).unwrap();
    assert!(text.contains("mode 0444") && text.contains(&hex(&profile_id("test-egress").0.0)), "{text}");
    assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o7777, 0o444);
    let (code, message) = run(&build).err().unwrap();
    assert!(code == 1 && message.contains("already exists"), "{message}");
    let verify = run(&args(&["verify", "--policy", out.to_str().unwrap(), "--artifacts-dir", artifacts.to_str().unwrap(),
        "--arch", "x86_64"])).unwrap();
    assert!(verify.starts_with("OK") && verify.contains("open_ai_compat") && verify.contains("runtime-api-worker-v1")
        && verify.contains("ABSENT") && verify.contains("egress"), "{verify}");
    // An invalid source writes nothing.
    let bad = fixture.write_source(&json!({"schema": SOURCE_SCHEMA}));
    let other = fixture.dir.path().join("other.json");
    let (code, _) = run(&args(&["build", "--source", bad.to_str().unwrap(), "--artifacts-dir", artifacts.to_str().unwrap(),
        "--arch", "x86_64", "--out", other.to_str().unwrap()])).err().unwrap();
    assert_eq!(code, 1);
    assert!(!other.exists());
    // Usage errors exit 2.
    for usage in [vec![], args(&["sign"]), args(&["build", "--source"]), args(&["verify", "--policy", "x", "--bogus", "y"]),
        args(&["build", "--source", "s", "--artifacts-dir", "d", "--arch", "riscv64", "--out", "o"])] {
        assert_eq!(run(&usage).err().unwrap().0, 2, "{usage:?}");
    }
}

#[test]
fn verify_binds_artifacts_and_a_signed_manifest() {
    let fixture = Fixture::new();
    let artifacts = fixture.artifacts();
    let (generated, _) = fixture.build(&fixture.source(), Arch::X86_64).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let policy_path = elsewhere.path().join("policy.json");
    fs::write(&policy_path, &generated.bytes).unwrap();
    let mut manifest = synthetic_manifest(Arch::X86_64, scan_artifacts(&artifacts, false).unwrap()).unwrap();
    manifest.artifacts.push(ReleaseArtifact { id: "isolation-policy-v1".into(), sha256: Sha256Digest(Sha256::digest(&generated.bytes).into()),
        size_bytes: generated.bytes.len() as u64, mode: 0o444, install_target: InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() } });
    let manifest_path = elsewhere.path().join("manifest.json");
    let write_manifest = |manifest: &ReleaseManifest| fs::write(&manifest_path, serde_json::to_vec(manifest).unwrap()).unwrap();
    let verify = |with_manifest: bool| {
        let mut command = args(&["verify", "--policy", policy_path.to_str().unwrap(), "--artifacts-dir", artifacts.to_str().unwrap()]);
        if with_manifest { command.extend(args(&["--manifest", manifest_path.to_str().unwrap()])); } else { command.extend(args(&["--arch", "x86_64"])); }
        run(&command)
    };
    write_manifest(&manifest);
    assert!(verify(true).is_ok(), "{:?}", verify(true).err());
    // The manifest's policy entry must be these very bytes.
    let mut changed = manifest.clone();
    changed.artifacts.last_mut().unwrap().sha256 = Sha256Digest([9; 32]);
    write_manifest(&changed);
    assert!(verify(true).err().unwrap().1.contains("isolation-policy-v1 artifact"));
    let mut missing = manifest.clone();
    missing.artifacts.pop();
    write_manifest(&missing);
    assert!(verify(true).err().unwrap().1.contains("no isolation-policy-v1"));
    // A runtime ELF that changed after the policy was generated.
    write_manifest(&manifest);
    fs::write(artifacts.join("runtime-api-worker-v1"), "\u{7f}ELF changed").unwrap();
    assert!(verify(true).err().unwrap().1.contains("differs from the manifest"));
    assert!(verify(false).err().unwrap().1.contains("validate_runtime_artifacts"));
    // A policy copy in --artifacts-dir must be the verified one.
    fs::write(artifacts.join("runtime-api-worker-v1"), API_WORKER).unwrap();
    assert!(verify(false).is_ok());
    fs::write(artifacts.join("runtime-isolation-policy-v1"), "{}").unwrap();
    assert!(verify(false).err().unwrap().1.contains("isolation-policy-v1 artifact"));
}

#[test]
fn mcp_acceptance_must_bind_the_exact_records() {
    let fixture = Fixture::new();
    let (_, policy) = fixture.build(&fixture.source(), Arch::X86_64).unwrap();
    let record = policy.runtimes[0].clone();
    let native = &policy.native[0];
    let launch = Sha256Digest(Sha256::digest(serde_json::to_vec(&json!({
        "domain": "duduclaw-runtime-mcp-acceptance-launch-v1", "runtime": record, "native": native })).unwrap()).into());
    let projection = duduclaw_mcp_boundary::config::runtime_projection(duduclaw_mcp_boundary::config::RuntimeRegistration {
        runtime: RuntimeKind::OpenAiCompat, version: "api-worker-v1-test".into(), runtime_digest: sha(API_WORKER),
        bridge_digest: sha(BRIDGE), bridge_executable: "/runtime/duduclaw-mcp-bridge".into() }).unwrap();
    let acceptance = McpAcceptance { runtime: RuntimeKind::OpenAiCompat, version: "api-worker-v1-test".into(),
        runtime_digest: sha(API_WORKER), bridge_digest: sha(BRIDGE), projection_digest: projection.digest(),
        launch_policy_digest: launch, inherited_fd: 3, initialize_exit: 0, parent_exec_exit: 0, mcp_child_exec_exit: 0,
        exclusive_config_exit: 0, transcript_digest: Sha256Digest([7; 32]) };
    let mut source = fixture.source();
    source["runtimes"][0]["mcp_acceptance"] = serde_json::to_value(&acceptance).unwrap();
    let (generated, accepted) = fixture.build(&source, Arch::X86_64).unwrap();
    assert!(accepted.runtimes[0].mcp_acceptance.is_some());
    assert!(summary(&accepted, &generated.bytes, Arch::X86_64).contains("present (checked)"));
    // A changed record (here: its resources) no longer matches the acceptance.
    let mut changed = source.clone();
    changed["runtimes"][0]["resources"]["tasks"] = 255.into();
    assert!(fixture.refusal(&changed).contains("validate_launch"));
    // An acceptance for another bridge.
    let mut other = source.clone();
    other["runtimes"][0]["mcp_acceptance"]["bridge_digest"] = serde_json::to_value(Sha256Digest([5; 32])).unwrap();
    assert!(fixture.refusal(&other).contains("McpAcceptance::validate (runtime"));
}
