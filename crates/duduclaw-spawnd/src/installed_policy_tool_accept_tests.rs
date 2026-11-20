//! R8r-4 acceptance harness: gates, the gateway-end responder, the decision,
//! the evidence encoding and `verify --evidence-dir`, the output source. All
//! unprivileged; the real launch is `iso_root_policy_accept_api_worker_end_to_end`.
use super::*;
use serde_json::json;
use std::collections::BTreeMap;

const API_WORKER: &str = "\u{7f}ELF TEST api worker bytes";
const FORWARDER: &str = "\u{7f}ELF TEST egress forwarder bytes";
const BRIDGE: &str = "\u{7f}ELF TEST mcp bridge bytes";
const TRAMPOLINE: &str = "\u{7f}ELF TEST trampoline";
const LABEL: &str = "api-worker-test";
const UID: u32 = 71_234;

fn trampoline_digest() -> Sha256Digest { sha(TRAMPOLINE.as_bytes()) }

struct Fixture { dir: tempfile::TempDir }
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = dir.path().join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        for (name, contents) in [("runtime-api-worker-v1", API_WORKER), ("duduclaw-egress-forwarder", FORWARDER),
            ("duduclaw-mcp-bridge", BRIDGE)] {
            fs::write(artifacts.join(name), contents).unwrap();
        }
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("x86_64.syscalls"), "# TEST-ONLY list\n0-334\n424-467\n").unwrap();
        fs::write(source.join("aarch64.syscalls"), "0-293\n424-467\n").unwrap();
        fs::create_dir(dir.path().join("out")).unwrap();
        fs::write(dir.path().join("duduclaw-run"), TRAMPOLINE).unwrap();
        let fixture = Fixture { dir };
        fixture.write_source(&fixture.source());
        fixture
    }
    fn seccomp() -> Value { json!({"x86_64": "x86_64.syscalls", "aarch64": "aarch64.syscalls"}) }
    fn runtime(label: &str, network: Value) -> Value {
        json!({
            "label": label, "runtime": {"kind": "open_ai_compat"}, "executable": "runtime-api-worker-v1",
            "pinned_version": "api-worker-v1-test", "fixed_args": "default",
            "resources": {"memory_bytes": 1073741824u64, "cpu_quota_percent": 100, "tasks": 256, "open_files": 4096,
                "ptys": 1, "disk": {"kind": "loop_filesystem", "bytes": 1073741824u64, "inodes": 65536},
                "io_read_bytes_per_sec": 52428800, "io_write_bytes_per_sec": 52428800, "io_read_ops_per_sec": 2000,
                "io_write_ops_per_sec": 2000, "output_buffer_bytes": 65536, "output_total_bytes": 16777216,
                "memfd_total_bytes": 8388608},
            "timeouts": {"queue_ms": 60000, "startup_ms": 30000, "runtime_ms": 600000, "pty_idle_ms": null,
                "termination_grace_ms": 10000},
            "network": network, "mcp": true, "pty_allowed": false, "queue_class": "background",
            "native": {"seccomp": Self::seccomp(), "tmpfs_bytes": 67108864, "tmpfs_inodes": 4096, "proc_subset_pid": true}
        })
    }
    /// The shipped TEST fixture's runtime plus a second (offline) api worker.
    fn source(&self) -> Value {
        json!({
            "schema": SOURCE_SCHEMA, "dns_probe_name": "example.com", "forwarder_seccomp": Self::seccomp(),
            "runtimes": [Self::runtime(LABEL, json!({"kind": "egress", "profile": "test-egress"})),
                Self::runtime("api-worker-second", json!({"kind": "offline"}))]
        })
    }
    fn source_path(&self) -> PathBuf { self.dir.path().join("source").join("source.json") }
    fn write_source(&self, source: &Value) { fs::write(self.source_path(), serde_json::to_vec_pretty(source).unwrap()).unwrap(); }
    fn artifacts(&self) -> PathBuf { self.dir.path().join("artifacts") }
    fn evidence(&self) -> PathBuf { self.dir.path().join("out").join("evidence") }
    fn out_source(&self) -> PathBuf { self.dir.path().join("out").join("accepted.json") }
    fn request(&self) -> Request {
        Request { source: self.source_path(), artifacts: self.artifacts(), arch: Arch::X86_64, label: LABEL.into(),
            trampoline: self.dir.path().join("duduclaw-run"), trampoline_sha256: trampoline_digest(), evidence: self.evidence(),
            out_source: self.out_source() }
    }
    fn args(&self) -> Vec<String> {
        let request = self.request();
        let trampoline_hex = hex(&trampoline_digest().0);
        ["--source", request.source.to_str().unwrap(), "--artifacts-dir", request.artifacts.to_str().unwrap(),
            "--arch", "x86_64", "--label", LABEL, "--trampoline", request.trampoline.to_str().unwrap(),
            "--evidence-dir", request.evidence.to_str().unwrap(), "--out-source", request.out_source.to_str().unwrap(),
            "--trampoline-sha256", trampoline_hex.as_str()]
            .iter().map(|value| (*value).to_owned()).collect()
    }
    /// The release manifest the packager would sign for these artifacts:
    /// the scanned entries plus the shipped trampoline (F1).
    fn release(&self) -> ReleaseManifest {
        let mut manifest = load_manifest(&self.artifacts(), None, Arch::X86_64, false).unwrap();
        manifest.artifacts.push(ReleaseArtifact { id: "duduclaw-run".into(), sha256: trampoline_digest(),
            size_bytes: TRAMPOLINE.len() as u64, mode: 0o755, install_target: InstallTarget::Trampoline });
        manifest
    }
    /// [`Self::release`] plus the policy entry, written as a `--manifest` file.
    fn release_file(&self, policy: &[u8]) -> PathBuf {
        let mut manifest = self.release();
        manifest.artifacts.push(ReleaseArtifact { id: "isolation-policy-v1".into(), sha256: sha(policy),
            size_bytes: policy.len() as u64, mode: 0o444, install_target: InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() } });
        let path = self.dir.path().join("release-manifest.json");
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        path
    }
    fn prepared(&self) -> Prepared {
        let raw = fs::read(self.source_path()).unwrap();
        prepare(&self.request(), &raw).unwrap()
    }
}

fn good_facts() -> HostFacts {
    HostFacts { linux: true, euid: 0, egid: 0, disposable: true, installed: false, protected_marker: false, arch: Some(Arch::X86_64) }
}
fn refusal_of(fixture: &Fixture, facts: &HostFacts) -> Option<(i32, String)> { command(&fixture.args(), facts).err() }
fn untouched(fixture: &Fixture) -> bool { !fixture.out_source().exists() && !fixture.evidence().exists() }
fn digest(byte: u8) -> Sha256Digest { Sha256Digest([byte; 32]) }
fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

// ── gates ───────────────────────────────────────────────────────────────

#[test]
fn host_gates_refuse_before_any_side_effect() {
    let fixture = Fixture::new();
    let cases: Vec<(fn(&mut HostFacts), &str)> = vec![
        (|f| f.linux = false, refusal::NOT_LINUX),
        (|f| f.euid = 1000, refusal::NOT_ROOT),
        (|f| f.egid = 5, refusal::NOT_ROOT),
        (|f| f.disposable = false, refusal::NOT_DISPOSABLE),
        (|f| f.installed = true, refusal::INSTALLED),
        (|f| f.protected_marker = true, refusal::INSTALLED),
        (|f| f.arch = Some(Arch::Aarch64), refusal::WRONG_ARCH),
        (|f| f.arch = None, refusal::WRONG_ARCH),
    ];
    for (mutate, expected) in cases {
        let mut facts = good_facts();
        mutate(&mut facts);
        assert_eq!(refusal_of(&fixture, &facts), Some((1, expected.to_owned())));
        assert!(untouched(&fixture));
    }
}

#[test]
fn path_gates_refuse_unusable_evidence_out_source_and_trampoline() {
    let fixture = Fixture::new();
    let facts = good_facts();
    // A non-empty evidence directory, a file, a symbolic link.
    fs::create_dir(fixture.evidence()).unwrap();
    fs::write(fixture.evidence().join("old"), "x").unwrap();
    assert_eq!(refusal_of(&fixture, &facts), Some((1, refusal::EVIDENCE_DIR.to_owned())));
    fs::remove_dir_all(fixture.evidence()).unwrap();
    fs::write(fixture.evidence(), "x").unwrap();
    assert_eq!(refusal_of(&fixture, &facts), Some((1, refusal::EVIDENCE_DIR.to_owned())));
    fs::remove_file(fixture.evidence()).unwrap();
    std::os::unix::fs::symlink(fixture.dir.path(), fixture.evidence()).unwrap();
    assert_eq!(refusal_of(&fixture, &facts), Some((1, refusal::EVIDENCE_DIR.to_owned())));
    fs::remove_file(fixture.evidence()).unwrap();
    // An existing --out-source (an empty evidence directory passes).
    fs::create_dir(fixture.evidence()).unwrap();
    fs::write(fixture.out_source(), "{}").unwrap();
    assert_eq!(refusal_of(&fixture, &facts), Some((1, refusal::OUT_SOURCE.to_owned())));
    fs::remove_file(fixture.out_source()).unwrap();
    // --out-source in a missing directory; --out-source = --source.
    let mut args = fixture.args();
    let position = args.iter().position(|arg| arg == "--out-source").unwrap();
    args[position + 1] = fixture.dir.path().join("missing").join("x.json").to_str().unwrap().to_owned();
    assert_eq!(command(&args, &facts).err(), Some((1, refusal::OUT_SOURCE.to_owned())));
    // A missing trampoline.
    fs::remove_file(fixture.dir.path().join("duduclaw-run")).unwrap();
    assert_eq!(refusal_of(&fixture, &facts), Some((1, refusal::TRAMPOLINE.to_owned())));
    assert!(!fixture.out_source().exists());
    assert_eq!(fs::read_dir(fixture.evidence()).unwrap().count(), 0);
    // Usage errors exit 2.
    assert_eq!(command(&["--source".to_owned()], &facts).err().map(|e| e.0), Some(2));
    let mut args = fixture.args();
    args[7] = "-bad-".into();
    assert_eq!(command(&args, &facts).err().map(|e| e.0), Some(2));
}

#[test]
fn label_gate_requires_an_api_worker_without_a_record() {
    let fixture = Fixture::new();
    let facts = good_facts();
    let gate = |source: &Value, label: &str| {
        fixture.write_source(source);
        let mut args = fixture.args();
        args[7] = label.to_owned();
        command(&args, &facts).err()
    };
    let source = fixture.source();
    assert_eq!(gate(&source, "no-such-runtime"), Some((1, refusal::LABEL_MISSING.to_owned())));
    let mut claude = source.clone();
    claude["runtimes"][0]["runtime"] = json!({"kind": "claude"});
    assert_eq!(gate(&claude, LABEL), Some((1, refusal::NOT_API_WORKER.to_owned())));
    let mut no_mcp = source.clone();
    no_mcp["runtimes"][0]["mcp"] = json!(false);
    assert_eq!(gate(&no_mcp, LABEL), Some((1, refusal::NOT_API_WORKER.to_owned())));
    let prepared = fixture.prepared_from(&source);
    let mut accepted = source.clone();
    accepted["runtimes"][0]["mcp_acceptance"] = serde_json::to_value(acceptance_record(&prepared, b"manifest")).unwrap();
    assert_eq!(gate(&accepted, LABEL), Some((1, refusal::ALREADY_ACCEPTED.to_owned())));
    assert!(untouched(&fixture));
}
impl Fixture {
    fn prepared_from(&self, source: &Value) -> Prepared {
        self.write_source(source);
        self.prepared()
    }
}

#[test]
fn installed_hosts_are_detected_from_any_install_fact() {
    let root = tempfile::tempdir().unwrap();
    assert!(!installed_under(root.path()));
    for fact in [INSTALL_FACTS[0], "/usr/lib/duduclaw/duduclaw-run", "/etc/systemd/system/duduclaw-spawnd.service"] {
        let root = tempfile::tempdir().unwrap();
        assert!(!installed_under(root.path()));
        let path = root.path().join(fact.trim_start_matches('/'));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        assert!(installed_under(root.path()), "{fact}");
    }
}

#[test]
fn install_facts_are_duduclaw_core_facts() {
    assert_eq!(INSTALL_FACTS, duduclaw_core::isolation_install::FACTS);
    assert_eq!(PROTECTED_MODE_MARKER, duduclaw_core::protected_mode::MODE_MARKER);
}

// ── the responder ───────────────────────────────────────────────────────

fn conversation() -> Vec<Frame> {
    let mut responder = Responder::default();
    assert!(responder.feed(&line(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))).unwrap().is_empty());
    responder.release_initialize(true).unwrap();
    assert!(responder.feed(&line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))).unwrap().is_empty());
    responder.feed(&line(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))).unwrap();
    assert!(responder.complete());
    responder.frames().to_vec()
}

#[test]
fn responder_holds_initialize_then_serves_an_empty_tool_list() {
    let mut responder = Responder::default();
    let initialize = line(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    assert!(responder.feed(&initialize[..7]).unwrap().is_empty());
    assert!(!responder.initialize_held());
    assert!(responder.feed(&initialize[7..]).unwrap().is_empty());
    assert!(responder.initialize_held());
    // A frame pipelined behind the held request waits for its reply.
    assert!(responder.feed(&line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))).unwrap().is_empty());
    let reply = responder.release_initialize(true).unwrap();
    assert!(reply.ends_with(b"\n"));
    let reply: Value = serde_json::from_slice(&reply[..reply.len() - 1]).unwrap();
    assert_eq!((reply["jsonrpc"].clone(), reply["id"].clone()), (json!("2.0"), json!(1)));
    assert!(reply["result"]["capabilities"].is_object() && reply.get("error").is_none());
    assert!(responder.release_initialize(true).is_err());
    let list = responder.feed(&line(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))).unwrap();
    let list: Value = serde_json::from_slice(&list[..list.len() - 1]).unwrap();
    assert_eq!((list["id"].clone(), list["result"].clone()), (json!(2), json!({"tools": []})));
    assert!(responder.complete());
    let facts = responder.facts();
    assert_eq!((facts.initialize_requests, facts.observed_before_reply, facts.initialized_after_reply,
        facts.tools_list_answered, facts.refused_frames, facts.frames), (1, true, true, true, 0, 5));
    let directions: Vec<Direction> = responder.frames().iter().map(|frame| frame.direction).collect();
    assert_eq!(directions, [Direction::RuntimeToHarness, Direction::HarnessToRuntime, Direction::RuntimeToHarness,
        Direction::RuntimeToHarness, Direction::HarnessToRuntime]);
    assert_eq!(responder.frames()[0].payload, initialize[..initialize.len() - 1].to_vec());
}

#[test]
fn responder_refuses_everything_else() {
    let error = |bytes: Vec<u8>| -> Value { serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap() };
    let mut responder = Responder::default();
    let refused = error(responder.feed(&line(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}))).unwrap());
    assert_eq!((refused["id"].clone(), refused["error"]["code"].clone()), (json!(7), json!(-32601)));
    let refused = error(responder.feed(b"not json\n").unwrap());
    assert_eq!((refused["id"].clone(), refused["error"]["code"].clone()), (Value::Null, json!(-32700)));
    assert!(responder.feed(&line(json!({"jsonrpc": "2.0", "method": "notifications/cancelled"}))).unwrap().is_empty());
    let refused = error(responder.feed(&line(json!({"jsonrpc": "1.0", "id": 1, "method": "initialize"}))).unwrap());
    assert_eq!(refused["error"]["code"], json!(-32700));
    // notifications/initialized before any initialize reply is refused, too.
    assert!(responder.feed(&line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))).unwrap().is_empty());
    let facts = responder.facts();
    assert_eq!((facts.refused_frames, facts.initialize_requests, facts.initialized_after_reply), (5, 0, false));
    // After the conversation: tools/call and a second initialize are refused and counted.
    let mut responder = Responder::default();
    responder.feed(&line(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))).unwrap();
    responder.release_initialize(false).unwrap();
    responder.feed(&line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))).unwrap();
    responder.feed(&line(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))).unwrap();
    let call = error(responder.feed(&line(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "x", "arguments": {}}}))).unwrap());
    assert!(call.get("error").is_some() && call.get("result").is_none());
    let again = error(responder.feed(&line(json!({"jsonrpc": "2.0", "id": 4, "method": "initialize"}))).unwrap());
    assert!(again.get("error").is_some());
    let facts = responder.facts();
    assert_eq!((facts.initialize_requests, facts.refused_frames, facts.observed_before_reply), (2, 2, false));
}

#[test]
fn responder_enforces_the_frame_and_byte_limits() {
    let notification = line(json!({"jsonrpc": "2.0", "method": "x"}));
    let mut responder = Responder::default();
    for _ in 0..MAX_FRAMES { assert!(responder.feed(&notification).is_ok()); }
    assert_eq!(responder.feed(&notification), Err(LIMIT_FRAMES));
    let mut responder = Responder::default();
    assert_eq!(responder.feed(&vec![b' '; MAX_TRANSCRIPT_BYTES + 1]), Err(LIMIT_BYTES));
    let mut responder = Responder::default();
    assert!(responder.feed(&vec![b' '; MAX_TRANSCRIPT_BYTES - 10]).is_ok());
    assert_eq!(responder.feed(&[b' '; 20]), Err(LIMIT_BYTES));
    assert_eq!(CONVERSATION_LIMIT, Duration::from_secs(30));
}

// ── the decision ────────────────────────────────────────────────────────

fn process(pid: u32, depth: u32, exe: Sha256Digest, fds: Vec<i32>) -> ProcessFacts {
    ProcessFacts { pid, depth, uids: [UID; 4], exe_sha256: exe, cgroup_member: true, socket_fds: fds, alive_through_observation: true }
}
fn valid() -> Observations {
    Observations { uid: UID, socket: SocketId { inode: 4242, mount_id: 9 }, runtime_digest: digest(3), bridge_digest: digest(4),
        trampoline_digest: digest(2), launcher_exe_sha256: Some(digest(2)),
        runtime: Some(process(100, 2, digest(3), vec![3])), runtime_fd3: Fd3::ExpectedSocket,
        bridge: Some(process(101, 1, digest(4), vec![5, 6])),
        initialize: InitializeFacts { initialize_requests: 1, observed_before_reply: true, initialized_after_reply: true,
            tools_list_answered: true, refused_frames: 0, frames: 5 },
        exclusive: ExclusiveFacts { projection_bytes_exact: true, projection_is_api_worker_config: true, projection_mount_id: 77,
            decoys_planted: 2, decoy_markers_present: 0, bridge_processes_seen: 1 },
        run_end: "exited 1".into(), incomplete: None }
}
/// [`valid`] with the fixture's real digests (what a genuine run of it observes).
fn observed(prepared: &Prepared) -> Observations {
    let mut o = valid();
    o.runtime_digest = prepared.runtime_digest;
    o.bridge_digest = prepared.bridge_digest;
    o.trampoline_digest = trampoline_digest();
    o.launcher_exe_sha256 = Some(trampoline_digest());
    o.runtime.as_mut().unwrap().exe_sha256 = prepared.runtime_digest;
    o.bridge.as_mut().unwrap().exe_sha256 = prepared.bridge_digest;
    o
}

#[test]
fn every_missing_or_false_fact_means_no_record() {
    assert!(decide(&valid()).is_ok());
    // fd 3 may be absent from the runtime when the tree still holds the socket.
    let mut absent = valid();
    absent.runtime_fd3 = Fd3::Absent;
    absent.runtime.as_mut().unwrap().socket_fds.clear();
    assert!(decide(&absent).is_ok());
    let mutations: Vec<(&str, fn(&mut Observations))> = vec![
        ("socket inode", |o| o.socket.inode = 0),
        ("socket mount", |o| o.socket.mount_id = 0),
        ("uid range", |o| o.uid = 1000),
        ("same digests", |o| o.bridge_digest = o.runtime_digest),
        ("launcher missing", |o| o.launcher_exe_sha256 = None),
        ("launcher other", |o| o.launcher_exe_sha256 = Some(digest(9))),
        ("runtime missing", |o| o.runtime = None),
        ("runtime exe", |o| o.runtime.as_mut().unwrap().exe_sha256 = digest(9)),
        ("runtime uid", |o| o.runtime.as_mut().unwrap().uids[2] = 0),
        ("runtime cgroup", |o| o.runtime.as_mut().unwrap().cgroup_member = false),
        ("runtime depth", |o| o.runtime.as_mut().unwrap().depth = 0),
        ("runtime pinned", |o| o.runtime.as_mut().unwrap().alive_through_observation = false),
        ("runtime fd 3", |o| o.runtime_fd3 = Fd3::Other),
        ("no socket in the tree", |o| { o.runtime.as_mut().unwrap().socket_fds.clear(); o.bridge.as_mut().unwrap().socket_fds.clear(); }),
        ("bridge missing", |o| o.bridge = None),
        ("bridge exe", |o| o.bridge.as_mut().unwrap().exe_sha256 = digest(3)),
        ("bridge uid", |o| o.bridge.as_mut().unwrap().uids[0] = 0),
        ("bridge cgroup", |o| o.bridge.as_mut().unwrap().cgroup_member = false),
        ("bridge depth", |o| o.bridge.as_mut().unwrap().depth = 0),
        ("bridge pinned", |o| o.bridge.as_mut().unwrap().alive_through_observation = false),
        ("bridge socket", |o| o.bridge.as_mut().unwrap().socket_fds.clear()),
        ("no initialize", |o| o.initialize.initialize_requests = 0),
        ("two initialize", |o| o.initialize.initialize_requests = 2),
        ("not observed first", |o| o.initialize.observed_before_reply = false),
        ("no initialized", |o| o.initialize.initialized_after_reply = false),
        ("no tools/list", |o| o.initialize.tools_list_answered = false),
        ("refused frame", |o| o.initialize.refused_frames = 1),
        ("projection bytes", |o| o.exclusive.projection_bytes_exact = false),
        ("projection config", |o| o.exclusive.projection_is_api_worker_config = false),
        ("projection mount", |o| o.exclusive.projection_mount_id = 0),
        ("decoy missing", |o| o.exclusive.decoys_planted = 1),
        ("decoy ran", |o| o.exclusive.decoy_markers_present = 1),
        ("no bridge seen", |o| o.exclusive.bridge_processes_seen = 0),
        ("two bridges", |o| o.exclusive.bridge_processes_seen = 2),
        ("incomplete observation", |o| o.incomplete = Some(incomplete::UNEXPECTED_PROCESS)),
    ];
    for (name, mutate) in mutations {
        let mut observations = valid();
        mutate(&mut observations);
        assert!(decide(&observations).is_err(), "{name}");
    }
}

// ── evidence and the record ─────────────────────────────────────────────

/// `(transcript, manifest, record)` of a run with these frames and
/// observations; `edit` changes the document before it is encoded (the
/// record then names the edited bytes).
fn evidence_from(prepared: &Prepared, frames: &[Frame], observations: &Observations, edit: impl FnOnce(&mut EvidenceDocument))
    -> (Vec<u8>, Vec<u8>, McpAcceptance) {
    let transcript = transcript_jsonl(frames);
    let mut document = evidence_document(&EvidenceSummary { arch: Arch::X86_64, harness_sha256: digest(1),
        trampoline_sha256: trampoline_digest(), runtime_sha256: prepared.runtime_digest, bridge_sha256: prepared.bridge_digest,
        projection_sha256: prepared.projection.digest(), launch_policy_sha256: prepared.launch_policy_digest,
        transcript_sha256: sha(&transcript), observations, kernel: "6.8.0-test", finished_unix: 1 });
    edit(&mut document);
    let manifest = encode_evidence(&document);
    let record = acceptance_record(prepared, &manifest);
    (transcript, manifest, record)
}
fn evidence(prepared: &Prepared) -> (Vec<u8>, Vec<u8>, McpAcceptance) {
    evidence_from(prepared, &conversation(), &observed(prepared), |_| {})
}

#[test]
fn canonical_json_and_base64_are_exact() {
    assert_eq!(canonical(&json!({"b": 1, "a": {"d": [1, {"z": true, "y": null}], "c": "x\n"}})),
        br#"{"a":{"c":"x\n","d":[1,{"y":null,"z":true}]},"b":1}"#.to_vec());
    for (plain, encoded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
        assert_eq!(base64(plain.as_bytes()), encoded);
        assert_eq!(base64_decode(encoded).unwrap(), plain.as_bytes());
    }
    for bad in ["Zg", "Zg=a", "Z===", "Zh==", "Zm9v!A==", "=Zm9"] { assert!(base64_decode(bad).is_none(), "{bad}"); }
}

#[test]
fn manifest_binds_the_transcript_and_its_digest_is_the_record() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared();
    // The generated record: the files' digests, the production projection and launch digests.
    assert_eq!(prepared.runtime_digest, sha(API_WORKER.as_bytes()));
    assert_eq!(prepared.bridge_digest, sha(BRIDGE.as_bytes()));
    assert_eq!(prepared.launch_policy_digest, McpAcceptance::launch_policy_digest(prepared.record(), prepared.native()).unwrap());
    let (transcript, manifest, record) = evidence(&prepared);
    let value: Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(canonical(&value), manifest);
    assert_eq!((value["domain"].clone(), value["format"].clone(), value["arch"].clone()),
        (json!(EVIDENCE_DOMAIN), json!(1), json!("x86_64")));
    assert_eq!(value["transcript_sha256"], json!(hex(&sha(&transcript).0)));
    assert_eq!(value["runtime_sha256"], json!(hex(&prepared.runtime_digest.0)));
    assert!(value["observations"]["parent_exec"].is_object() && value["observations"]["exclusive_config"]["decoys_planted"] == json!(2));
    assert_eq!(record.transcript_digest, sha(&manifest));
    assert_eq!((record.inherited_fd, record.initialize_exit, record.parent_exec_exit, record.mcp_child_exec_exit,
        record.exclusive_config_exit), (3, 0, 0, 0, 0));
    assert!(record.validate(&prepared.projection).is_ok());
    assert!(record.validate_launch(prepared.record(), prepared.native()).is_ok());
    // Every transcript line is canonical, in sequence, with its payload's digest.
    assert_eq!(transcript.iter().filter(|byte| **byte == b'\n').count(), 5);
    let first: Value = serde_json::from_slice(transcript.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
    assert_eq!((first["seq"].clone(), first["direction"].clone()), (json!(1), json!("runtime_to_harness")));
}

#[test]
fn verify_evidence_dir_accepts_only_the_matching_evidence() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let raw = fs::read(&request.source).unwrap();
    let prepared = prepare(&request, &raw).unwrap();
    let (transcript, manifest, record) = evidence(&prepared);
    let out = accepted_source(&raw, &request.source, &request.out_source, LABEL, &record).unwrap();
    let (generated, policy) = build_policy_bytes(&out, &request.out_source, &request.artifacts, Arch::X86_64, None).unwrap();
    let release = fixture.release();
    let evidence_dir = fixture.dir.path().join("evidence");
    let label_dir = evidence_dir.join(LABEL);
    fs::create_dir_all(&label_dir).unwrap();
    fs::write(label_dir.join(TRANSCRIPT_FILE), &transcript).unwrap();
    fs::write(label_dir.join(MANIFEST_FILE), &manifest).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir, None).unwrap().contains("1 acceptance record(s)"));
    // Through the command line: --evidence-dir always with the release manifest.
    let policy_path = fixture.dir.path().join("policy.json");
    fs::write(&policy_path, &generated.bytes).unwrap();
    let release_path = fixture.release_file(&generated.bytes);
    let verify = |extra: &[&str]| {
        let mut args: Vec<String> = ["verify", "--policy", policy_path.to_str().unwrap(), "--artifacts-dir",
            request.artifacts.to_str().unwrap()].iter().map(|v| (*v).to_owned()).collect();
        args.extend(extra.iter().map(|v| (*v).to_owned()));
        super::super::run(&args)
    };
    let with_manifest = ["--manifest", release_path.to_str().unwrap()];
    assert!(verify(&["--arch", "x86_64"]).is_ok());
    assert!(verify(&with_manifest).is_ok(), "{:?}", verify(&with_manifest).err());
    let evidence_args = ["--manifest", release_path.to_str().unwrap(), "--evidence-dir", evidence_dir.to_str().unwrap()];
    assert!(verify(&evidence_args).unwrap().contains("backed by harness evidence"));
    let pin = hex(&digest(1).0);
    let pinned: Vec<&str> = [&evidence_args[..], &["--harness-sha256", pin.as_str()][..]].concat();
    assert!(verify(&pinned).unwrap().contains("the pinned harness"));
    let other_pin = hex(&digest(7).0);
    let mispinned: Vec<&str> = [&evidence_args[..], &["--harness-sha256", other_pin.as_str()][..]].concat();
    assert!(verify(&mispinned).err().unwrap().1.contains(evidence_refusal::HARNESS_PIN));
    let malformed: Vec<&str> = [&evidence_args[..], &["--harness-sha256", "ABC"][..]].concat();
    assert_eq!(verify(&malformed).err().map(|e| e.0), Some(2));
    assert_eq!(verify(&["--arch", "x86_64", "--harness-sha256", pin.as_str()]).err().map(|e| e.0), Some(2));
    // No evidence at all.
    let empty = fixture.dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert_eq!(verify(&["--manifest", release_path.to_str().unwrap(), "--evidence-dir", empty.to_str().unwrap()]).err().map(|e| e.0),
        Some(1));
    // A tampered transcript.
    let mut tampered = transcript.clone();
    tampered[10] ^= 1;
    fs::write(label_dir.join(TRANSCRIPT_FILE), &tampered).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir, None).is_err());
    fs::write(label_dir.join(TRANSCRIPT_FILE), &transcript).unwrap();
    // A tampered manifest (no longer the record's transcript_digest).
    let mut tampered = manifest.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    fs::write(label_dir.join(MANIFEST_FILE), &tampered).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir, None).is_err());
    fs::write(label_dir.join(MANIFEST_FILE), &manifest).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir, None).is_ok());
    // Artifacts whose digests differ from the record and the manifest.
    let accepted = policy.runtimes.iter().find(|r| r.mcp_acceptance.is_some()).unwrap();
    let acceptance = accepted.mcp_acceptance.as_ref().unwrap();
    let check = |release: &ReleaseManifest, arch: Arch, transcript: &[u8]|
        check_evidence(&manifest, transcript, acceptance, accepted, release, arch, None);
    assert_eq!(check(&release, Arch::X86_64, &transcript), Ok(()));
    let mut other = release.clone();
    for artifact in &mut other.artifacts {
        if artifact.id == "runtime-api-worker-v1" { artifact.sha256 = digest(9); }
    }
    assert_eq!(check(&other, Arch::X86_64, &transcript), Err(evidence_refusal::ARTIFACTS));
    let mut other = release.clone();
    for artifact in &mut other.artifacts {
        if artifact.install_target == InstallTarget::McpBridge { artifact.sha256 = digest(9); }
    }
    assert_eq!(check(&other, Arch::X86_64, &transcript), Err(evidence_refusal::ARTIFACTS));
    assert_eq!(check(&release, Arch::Aarch64, &transcript), Err(evidence_refusal::ARCH));
    assert_eq!(check(&release, Arch::X86_64, b""), Err(evidence_refusal::TRANSCRIPT_FILE));
}

// ── F1: the trampoline is the shipped one ───────────────────────────────

#[test]
fn accept_refuses_a_trampoline_whose_digest_differs() {
    let fixture = Fixture::new();
    let facts = good_facts();
    let mut args = fixture.args();
    let position = args.iter().position(|arg| arg == "--trampoline-sha256").unwrap();
    // Another digest than the file's: refused before any side effect.
    args[position + 1] = hex(&digest(9).0);
    assert_eq!(command(&args, &facts).err(), Some((1, refusal::TRAMPOLINE_DIGEST.to_owned())));
    assert!(untouched(&fixture));
    // The file changed after its digest was computed.
    fs::write(fixture.dir.path().join("duduclaw-run"), "\u{7f}ELF TEST another trampoline").unwrap();
    assert_eq!(refusal_of(&fixture, &facts), Some((1, refusal::TRAMPOLINE_DIGEST.to_owned())));
    assert!(untouched(&fixture));
    // Missing or malformed: a usage error.
    let bad_values: [String; 4] = [String::new(), "ABCD".into(), "0".repeat(63), "A".repeat(64)];
    for bad in &bad_values {
        let mut args = fixture.args();
        args[position + 1] = bad.clone();
        assert_eq!(command(&args, &facts).err().map(|e| e.0), Some(2), "{bad:?}");
    }
    let args: Vec<String> = fixture.args()[..position].to_vec();
    assert_eq!(command(&args, &facts).err().map(|e| e.0), Some(2));
    assert!(parse_digest_hex(&hex(&digest(0xab).0)) == Some(digest(0xab)));
}

#[test]
fn verify_refuses_evidence_whose_trampoline_is_not_the_manifests() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared();
    let (transcript, manifest, record) = evidence(&prepared);
    let release = fixture.release();
    let check = |release: &ReleaseManifest| check_evidence(&manifest, &transcript, &record, prepared.record(), release,
        Arch::X86_64, None);
    assert_eq!(check(&release), Ok(()));
    let mut other = release.clone();
    other.artifacts.iter_mut().find(|a| a.install_target == InstallTarget::Trampoline).unwrap().sha256 = digest(9);
    assert_eq!(check(&other), Err(evidence_refusal::TRAMPOLINE));
    let mut none = release.clone();
    none.artifacts.retain(|a| a.install_target != InstallTarget::Trampoline);
    assert_eq!(check(&none), Err(evidence_refusal::NO_TRAMPOLINE));
    // Evidence of a run with another trampoline than the shipped one.
    let mut observations = observed(&prepared);
    observations.trampoline_digest = digest(9);
    observations.launcher_exe_sha256 = Some(digest(9));
    let (transcript, manifest, record) = evidence_from(&prepared, &conversation(), &observations,
        |document| document.trampoline_sha256 = hex(&digest(9).0));
    assert_eq!(check_evidence(&manifest, &transcript, &record, prepared.record(), &release, Arch::X86_64, None),
        Err(evidence_refusal::TRAMPOLINE));
}

#[test]
fn verify_evidence_dir_requires_a_manifest() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let raw = fs::read(&request.source).unwrap();
    let (generated, _) = build_policy_bytes(&raw, &request.source, &request.artifacts, Arch::X86_64, None).unwrap();
    let policy_path = fixture.dir.path().join("policy.json");
    fs::write(&policy_path, &generated.bytes).unwrap();
    let evidence_dir = fixture.dir.path().join("evidence-dir");
    fs::create_dir(&evidence_dir).unwrap();
    let args: Vec<String> = ["verify", "--policy", policy_path.to_str().unwrap(), "--artifacts-dir", request.artifacts.to_str().unwrap(),
        "--arch", "x86_64", "--evidence-dir", evidence_dir.to_str().unwrap()].iter().map(|v| (*v).to_owned()).collect();
    assert_eq!(super::super::run(&args).err(), Some((1, EVIDENCE_NEEDS_MANIFEST.to_owned())));
}

// ── F4: the evidence is parsed, replayed and decided again ──────────────

#[test]
fn evidence_manifest_is_parsed_strictly_and_decided_again() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared();
    let release = fixture.release();
    let check = |transcript: &[u8], manifest: &[u8], record: &McpAcceptance, pin: Option<Sha256Digest>|
        check_evidence(manifest, transcript, record, prepared.record(), &release, Arch::X86_64, pin);
    // The genuine fixture passes, pinned or not.
    let (transcript, manifest, record) = evidence(&prepared);
    assert_eq!(check(&transcript, &manifest, &record, None), Ok(()));
    assert_eq!(check(&transcript, &manifest, &record, Some(digest(1))), Ok(()));
    assert_eq!(check(&transcript, &manifest, &record, Some(digest(7))), Err(evidence_refusal::HARNESS_PIN));
    // Value-level edits, re-bound as the record's transcript_digest.
    let original: Value = serde_json::from_slice(&manifest).unwrap();
    let edited = |edit: &dyn Fn(&mut Value), canonical_bytes: bool| {
        let mut value = original.clone();
        edit(&mut value);
        let bytes = if canonical_bytes { canonical(&value) } else { serde_json::to_vec_pretty(&value).unwrap() };
        let record = acceptance_record(&prepared, &bytes);
        check(&transcript, &bytes, &record, None)
    };
    assert_eq!(edited(&|v: &mut Value| { v.as_object_mut().unwrap().remove("harness_sha256"); }, true), Err(evidence_refusal::SCHEMA));
    assert_eq!(edited(&|v: &mut Value| { v.as_object_mut().unwrap().remove("observations"); }, true), Err(evidence_refusal::SCHEMA));
    assert_eq!(edited(&|v: &mut Value| { v["observations"]["initialize"].as_object_mut().unwrap().remove("frames"); }, true),
        Err(evidence_refusal::SCHEMA));
    assert_eq!(edited(&|v: &mut Value| { v["extra"] = json!(1); }, true), Err(evidence_refusal::SCHEMA));
    assert_eq!(edited(&|v: &mut Value| { v["observations"]["exclusive_config"]["extra"] = json!(true); }, true),
        Err(evidence_refusal::SCHEMA));
    assert_eq!(edited(&|v: &mut Value| { v["format"] = json!("1"); }, true), Err(evidence_refusal::SCHEMA));
    // A missing nullable field parses but does not re-encode to the same bytes.
    assert_eq!(edited(&|v: &mut Value| { v["observations"]["parent_exec"].as_object_mut().unwrap().remove("launcher_exe_sha256"); },
        true), Err(evidence_refusal::CANONICAL));
    assert_eq!(edited(&|_: &mut Value| {}, false), Err(evidence_refusal::CANONICAL));
    // Typed edits.
    let typed = |edit: fn(&mut EvidenceDocument)| {
        let (transcript, manifest, record) = evidence_from(&prepared, &conversation(), &observed(&prepared), edit);
        check(&transcript, &manifest, &record, None)
    };
    assert_eq!(typed(|d| d.domain = "another".into()), Err(evidence_refusal::DOMAIN));
    assert_eq!(typed(|d| d.format = EVIDENCE_FORMAT + 1), Err(evidence_refusal::FORMAT));
    assert_eq!(typed(|d| d.arch = "aarch64".into()), Err(evidence_refusal::ARCH));
    assert_eq!(typed(|d| d.harness_sha256 = "00".into()), Err(evidence_refusal::DIGEST_SYNTAX));
    assert_eq!(typed(|d| d.harness_sha256 = "AB".repeat(32)), Err(evidence_refusal::DIGEST_SYNTAX));
    assert_eq!(typed(|d| d.bridge_sha256 = hex(&[9; 32])), Err(evidence_refusal::RECORD_DIGESTS));
    assert_eq!(typed(|d| d.transcript_sha256 = hex(&[9; 32])), Err(evidence_refusal::TRANSCRIPT_FILE));
    assert_eq!(typed(|d| d.observations.initialize.frames = 6), Err(evidence_refusal::TRANSCRIPT_FACTS));
    assert_eq!(typed(|d| d.observations.exclusive_config.initialize_requests = 2), Err(evidence_refusal::TRANSCRIPT_FACTS));
    assert_eq!(typed(|d| d.observations.parent_exec.fd3 = "maybe".into()), Err(evidence_refusal::SCHEMA));
    // A false observation: the same decide refuses it.
    let false_facts: Vec<fn(&mut Observations)> = vec![
        |o| o.exclusive.decoy_markers_present = 1,
        |o| o.exclusive.bridge_processes_seen = 2,
        |o| o.runtime.as_mut().unwrap().alive_through_observation = false,
        |o| o.launcher_exe_sha256 = None,
        |o| o.initialize.observed_before_reply = false,
        |o| o.bridge = None,
    ];
    for mutate in false_facts {
        let mut observations = observed(&prepared);
        mutate(&mut observations);
        let (transcript, manifest, record) = evidence_from(&prepared, &conversation(), &observations, |_| {});
        assert_eq!(check(&transcript, &manifest, &record, None), Err(evidence_refusal::DECISION));
    }
    // A record that does not state zero exits.
    let (transcript, manifest, mut record) = evidence(&prepared);
    record.parent_exec_exit = 1;
    assert_eq!(check(&transcript, &manifest, &record, None), Err(evidence_refusal::RECORD_EXITS));
}

/// Frames the responder produces for these runtime messages (the held
/// `initialize` released as the harness does).
fn frames_of(messages: &[Value]) -> Vec<Frame> {
    let mut responder = Responder::default();
    for message in messages {
        responder.feed(&line(message.clone())).unwrap();
        if responder.initialize_held() { responder.release_initialize(true).unwrap(); }
    }
    responder.frames().to_vec()
}

#[test]
fn transcript_is_replayed_through_the_responder() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared();
    let release = fixture.release();
    let check = |frames: &[Frame]| {
        let (transcript, manifest, record) = evidence_from(&prepared, frames, &observed(&prepared), |_| {});
        check_evidence(&manifest, &transcript, &record, prepared.record(), &release, Arch::X86_64, None)
    };
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
    assert_eq!(frames_of(&[initialize.clone(), initialized.clone(), list.clone()]), conversation());
    assert_eq!(check(&conversation()), Ok(()));
    // Out of order: notifications/initialized and tools/list swapped.
    let mut swapped = conversation();
    swapped.swap(2, 3);
    assert_eq!(check(&swapped), Err(evidence_refusal::TRANSCRIPT_ORDER));
    // A harness frame the responder never writes.
    let mut forged = conversation();
    forged[4].payload = br#"{"id":2,"jsonrpc":"2.0","result":{"tools":[{"name":"x"}]}}"#.to_vec();
    assert_eq!(check(&forged), Err(evidence_refusal::TRANSCRIPT_ORDER));
    // Incomplete: no tools/list.
    assert_eq!(check(&frames_of(&[initialize.clone(), initialized.clone()])), Err(evidence_refusal::TRANSCRIPT_ORDER));
    // A second initialize after the conversation.
    let again = json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {}});
    assert_eq!(check(&frames_of(&[initialize.clone(), initialized.clone(), list.clone(), again])),
        Err(evidence_refusal::SECOND_INITIALIZE));
    // A refused frame in the middle.
    let ping = json!({"jsonrpc": "2.0", "id": 9, "method": "ping"});
    assert_eq!(check(&frames_of(&[initialize.clone(), initialized.clone(), ping, list.clone()])),
        Err(evidence_refusal::REFUSED_FRAME));
    // Line-level damage: a non-canonical line, a wrong sequence number.
    let (transcript, manifest, record) = evidence(&prepared);
    let rebind = |transcript: Vec<u8>| {
        let (_, manifest, record) = evidence_from(&prepared, &conversation(), &observed(&prepared),
            |d| d.transcript_sha256 = hex(&sha(&transcript).0));
        check_evidence(&manifest, &transcript, &record, prepared.record(), &release, Arch::X86_64, None)
    };
    let text = String::from_utf8(transcript.clone()).unwrap();
    assert_eq!(rebind(text.replacen("{\"direction\"", "{ \"direction\"", 1).into_bytes()), Err(evidence_refusal::TRANSCRIPT_LINES));
    assert_eq!(rebind(text.replacen("\"seq\":2", "\"seq\":3", 1).into_bytes()), Err(evidence_refusal::TRANSCRIPT_LINES));
    assert_eq!(rebind(transcript[..transcript.len() - 1].to_vec()), Err(evidence_refusal::TRANSCRIPT_TRUNCATED));
    assert_eq!(check_evidence(&manifest, &transcript, &record, prepared.record(), &release, Arch::X86_64, None), Ok(()));
}

// ── F2: complete enumeration, fail-closed classification ────────────────

const LAUNCHER: u32 = 10;
fn run_ids(forwarder: bool) -> Identities {
    Identities { launcher: (LAUNCHER, start_of(LAUNCHER)), trampoline: digest(2), runtime: digest(3), bridge: digest(4),
        forwarder: forwarder.then(|| digest(5)) }
}
fn start_of(pid: u32) -> u64 { 1000 + u64::from(pid) }
fn live(pid: u32, ppid: u32, exe: Sha256Digest) -> Member {
    Member { pid, read: MemberRead::Live { start: start_of(pid), ppid, exe } }
}
/// The legitimate run: supervisor 10, init 11, forwarder 12, runtime 13, bridge 14.
fn run_members() -> Vec<Member> {
    vec![live(10, 1, digest(2)), live(11, 10, digest(2)), live(12, 11, digest(5)), live(13, 11, digest(3)), live(14, 13, digest(4))]
}
fn snapshot_of(members: Vec<Member>) -> CgroupSnapshot { CgroupSnapshot { members, incomplete: None } }
fn limits() -> WalkLimits { WalkLimits::for_tasks(256) }
/// What `accept` would decide on a snapshot: the run's facts with the
/// classification's and the census's verdict.
fn decision_on(snapshot: &CgroupSnapshot, ids: &Identities, limits: &WalkLimits) -> std::result::Result<(), &'static str> {
    let mut census = Census::default();
    census.record(snapshot, ids, limits, false, false);
    let mut observations = valid();
    observations.incomplete = classify(snapshot, ids, limits).err().or_else(|| census.finish(ids));
    decide(&observations)
}

#[test]
fn the_legitimate_run_cgroup_classifies_completely() {
    let ids = run_ids(true);
    let classified = classify(&snapshot_of(run_members()), &ids, &limits()).unwrap();
    assert_eq!((classified.runtime, classified.bridge), (Some((13, 2)), Some((14, 1))));
    assert_eq!(classified.live.len(), 5);
    assert_eq!(decision_on(&snapshot_of(run_members()), &ids, &limits()), Ok(()));
    // Offline: no forwarder.
    let offline: Vec<Member> = run_members().into_iter().filter(|m| m.pid != 12).collect();
    assert!(classify(&snapshot_of(offline), &run_ids(false), &limits()).is_ok());
    // The pre-exec windows and the documented exec transitions, over time.
    let mut census = Census::default();
    let ids = run_ids(true);
    let turns = [
        vec![live(10, 1, digest(2)), live(11, 10, digest(2)), live(12, 11, digest(2))],
        vec![live(10, 1, digest(2)), live(11, 10, digest(2)), live(12, 11, digest(5)), live(13, 11, digest(2))],
        vec![live(10, 1, digest(2)), live(11, 10, digest(2)), live(12, 11, digest(5)), live(13, 11, digest(3)), live(14, 13, digest(3))],
        run_members(),
    ];
    for turn in &turns { census.record(&snapshot_of(turn.clone()), &ids, &limits(), false, false); }
    assert_eq!((census.finish(&ids), census.bridges(&ids)), (None, 1));
    // The runtime exits first: the bridge is re-parented to init.
    let orphan = vec![live(10, 1, digest(2)), live(11, 10, digest(2)), live(12, 11, digest(5)), live(14, 11, digest(4))];
    census.record(&snapshot_of(orphan), &ids, &limits(), false, true);
    assert_eq!(census.finish(&ids), None);
    // After the main process exited (confirmed) and the conversation completed, nothing is classified.
    census.record(&snapshot_of(vec![live(99, 1, digest(9))]), &ids, &limits(), true, true);
    assert_eq!(census.finish(&ids), None);
    assert_eq!(WalkLimits::for_tasks(512), WalkLimits { max_nodes: 512, max_depth: 512, max_census: CENSUS_MAX });
}

#[test]
fn reaching_any_walk_limit_is_incomplete_not_truncated() {
    let ids = run_ids(true);
    let snapshot = snapshot_of(run_members());
    // Depth: the bridge is 3 below the main process.
    let shallow = WalkLimits { max_depth: 2, ..limits() };
    assert_eq!(classify(&snapshot, &ids, &shallow).err(), Some(incomplete::DEPTH_CAP));
    assert!(decision_on(&snapshot, &ids, &shallow).unwrap_err().contains(incomplete::DEPTH_CAP));
    // Nodes: five members, four allowed.
    let narrow = WalkLimits { max_nodes: 4, ..limits() };
    assert_eq!(classify(&snapshot, &ids, &narrow).err(), Some(incomplete::NODE_CAP));
    assert!(decision_on(&snapshot, &ids, &narrow).unwrap_err().contains(incomplete::NODE_CAP));
    // Census: five distinct processes, three kept.
    let small = WalkLimits { max_census: 3, ..limits() };
    let mut census = Census::default();
    census.record(&snapshot, &ids, &small, false, false);
    assert_eq!(census.finish(&ids), Some(incomplete::CENSUS_CAP));
    assert!(decision_on(&snapshot, &ids, &small).unwrap_err().contains(incomplete::CENSUS_CAP));
    // Enumeration-level gaps.
    for reason in [incomplete::CGROUP_UNREADABLE, incomplete::CGROUP_UNSTABLE, incomplete::CGROUP_TREE_CAP] {
        let gap = CgroupSnapshot { members: run_members(), incomplete: Some(reason) };
        assert!(decision_on(&gap, &ids, &limits()).unwrap_err().contains(reason));
    }
}

#[test]
fn unreadable_unhashable_or_unknown_processes_mean_no_record() {
    let ids = run_ids(true);
    let with = |pid: u32, read: MemberRead| {
        let mut members = run_members();
        members.retain(|m| m.pid != pid);
        members.push(Member { pid, read });
        snapshot_of(members)
    };
    let cases = [
        (with(14, MemberRead::Unreadable), incomplete::UNREADABLE_PROCESS),
        (with(14, MemberRead::HashError), incomplete::HASH_ERROR),
        (with(13, MemberRead::Exited { confirmed: false }), incomplete::UNCONFIRMED_EXIT),
        // An executable no manifest entry or the trampoline names.
        (snapshot_of([run_members(), vec![live(15, 13, digest(9))]].concat()), incomplete::UNEXPECTED_PROCESS),
        // A second runtime, a second forwarder, a second init.
        (snapshot_of([run_members(), vec![live(15, 11, digest(3))]].concat()), incomplete::UNEXPECTED_PROCESS),
        (snapshot_of([run_members(), vec![live(15, 11, digest(5))]].concat()), incomplete::UNEXPECTED_PROCESS),
        (snapshot_of([run_members(), vec![live(15, 10, digest(2))]].concat()), incomplete::UNEXPECTED_PROCESS),
        // A process in the cgroup that does not descend from the main process.
        (snapshot_of([run_members(), vec![live(15, 1, digest(4))]].concat()), incomplete::UNEXPECTED_PROCESS),
        // The bridge spawned by init, a forwarder below the runtime.
        (snapshot_of([run_members(), vec![live(15, 12, digest(4))]].concat()), incomplete::UNEXPECTED_PROCESS),
        (snapshot_of([run_members(), vec![live(15, 13, digest(5))]].concat()), incomplete::UNEXPECTED_PROCESS),
        // The main process missing or replaced.
        (snapshot_of(run_members().into_iter().filter(|m| m.pid != LAUNCHER).collect()), incomplete::LAUNCHER_MISSING),
        (with(LAUNCHER, MemberRead::Live { start: 1, ppid: 1, exe: digest(2) }), incomplete::UNEXPECTED_PROCESS),
    ];
    for (snapshot, reason) in cases {
        let decision = decision_on(&snapshot, &ids, &limits());
        assert!(decision.as_ref().unwrap_err().contains(reason), "{reason}: {decision:?}");
    }
    // A forwarder in an offline run.
    assert!(decision_on(&snapshot_of(run_members()), &run_ids(false), &limits()).unwrap_err().contains(incomplete::UNEXPECTED_PROCESS));
    // Census-only refusals: a second runtime over time; an undocumented exec.
    let ids = run_ids(true);
    let mut census = Census::default();
    census.record(&snapshot_of(run_members()), &ids, &limits(), false, false);
    let respawned = vec![live(10, 1, digest(2)), live(11, 10, digest(2)), live(12, 11, digest(5)), live(16, 11, digest(3)),
        live(14, 16, digest(4))];
    census.record(&snapshot_of(respawned), &ids, &limits(), false, false);
    assert_eq!(census.finish(&ids), Some(incomplete::UNEXPECTED_PROCESS));
    let mut census = Census::default();
    census.record(&snapshot_of(run_members()), &ids, &limits(), false, false);
    // The bridge's (pid, start) later shows the runtime digest: bridge → runtime is no documented exec.
    let mut reexec = run_members();
    reexec[4] = live(14, 13, digest(3));
    census.record(&snapshot_of(reexec), &ids, &limits(), false, false);
    assert_eq!(census.finish(&ids), Some(incomplete::UNEXPECTED_PROCESS));
    // The main process gone before the conversation completed.
    let mut census = Census::default();
    census.record(&snapshot_of(run_members()), &ids, &limits(), true, false);
    assert_eq!(census.finish(&ids), Some(incomplete::LAUNCHER_MISSING));
}

#[test]
fn exited_processes_are_tolerated_only_with_exit_confirmation() {
    let ids = run_ids(true);
    let mut members = run_members();
    members.push(Member { pid: 15, read: MemberRead::Exited { confirmed: true } });
    let classified = classify(&snapshot_of(members.clone()), &ids, &limits()).unwrap();
    assert_eq!((classified.runtime, classified.bridge), (Some((13, 2)), Some((14, 1))));
    assert_eq!(decision_on(&snapshot_of(members.clone()), &ids, &limits()), Ok(()));
    members.push(Member { pid: 16, read: MemberRead::Exited { confirmed: false } });
    assert_eq!(classify(&snapshot_of(members.clone()), &ids, &limits()).err(), Some(incomplete::UNCONFIRMED_EXIT));
    assert!(decision_on(&snapshot_of(members), &ids, &limits()).unwrap_err().contains(incomplete::UNCONFIRMED_EXIT));
}

// ── R8r-4 third real-host run: the observed process tree ────────────────

/// The run as sampled on the host: supervisor 42644 (the unit's MainPID,
/// parent PID 1, uid 0 at start then the supervisor id 988), init 42673,
/// forwarder 42674 and runtime 42675 below init, bridge 42677 below the
/// runtime; all but the supervisor run as the employee uid 76202, in ONE
/// flat cgroup.
const HOST_SUPERVISOR: u32 = 42644;
fn host_ids() -> Identities {
    Identities { launcher: (HOST_SUPERVISOR, start_of(HOST_SUPERVISOR)), trampoline: digest(2), runtime: digest(3), bridge: digest(4),
        forwarder: Some(digest(5)) }
}
fn host_members() -> Vec<Member> {
    vec![live(42644, 1, digest(2)), live(42673, 42644, digest(2)), live(42674, 42673, digest(5)), live(42675, 42673, digest(3)),
        live(42677, 42675, digest(4))]
}
fn host_notes(supervisor_uid: u32) -> BTreeMap<u32, MemberNote> {
    [(42644, supervisor_uid), (42673, 76202), (42674, 76202), (42675, 76202), (42677, 76202)].into_iter()
        .map(|(pid, uid)| (pid, MemberNote { uid: Some(uid), errno: None })).collect()
}

#[test]
fn the_host_ground_truth_classifies_completely_with_all_four_facts_obtainable() {
    let ids = host_ids();
    let snapshot = snapshot_of(host_members());
    assert_eq!(absent_parents(&snapshot, &ids), Vec::<u32>::new());
    let classified = classify_sited(&snapshot, &ids, &limits()).unwrap();
    // Facts a and b are read from exactly these two pidfd-pinned members.
    assert_eq!((classified.runtime, classified.bridge), (Some((42675, 2)), Some((42677, 1))));
    assert_eq!(classified.live.len(), 5);
    let mut census = Census::default();
    assert_eq!(census.record(&snapshot, &ids, &limits(), false, false), Turn::Classified);
    assert!(census.runtime_seen);
    assert_eq!((census.finish(&ids), census.bridges(&ids)), (None, 1));
    // With the runtime and bridge facts observed, all four facts decide.
    assert_eq!(decision_on(&snapshot, &ids, &limits()), Ok(()));
    assert_eq!(member_lines(&snapshot, &host_notes(988), &ids), [
        "42644 1 988 supervisor 02020202", "42673 42644 76202 init 02020202", "42674 42673 76202 forwarder 05050505",
        "42675 42673 76202 runtime 03030303", "42677 42675 76202 bridge 04040404"]);
}

#[test]
fn the_supervisor_changing_uid_after_start_changes_nothing() {
    // Sample 1 (activating): only the supervisor, still uid 0; then the tree,
    // the supervisor now uid 988. Uids are no census fact; the supervisor's
    // (pid, start) and digest are the same in both turns.
    let ids = host_ids();
    let first = snapshot_of(vec![live(42644, 1, digest(2))]);
    let later = snapshot_of(host_members());
    let mut census = Census::default();
    assert_eq!(census.record(&first, &ids, &limits(), false, false), Turn::Classified);
    assert_eq!(census.record(&later, &ids, &limits(), false, false), Turn::Classified);
    assert_eq!(census.finish(&ids), None);
    assert_eq!(census.seen.get(&(42644, start_of(42644))), Some(&vec![digest(2)]));
    assert_eq!(member_lines(&first, &host_notes(0), &ids), ["42644 1 0 supervisor 02020202"]);
    assert_eq!(member_lines(&later, &host_notes(988), &ids)[0], "42644 1 988 supervisor 02020202");
    assert_eq!(decision_on(&later, &ids, &limits()), Ok(()));
}

#[test]
fn a_supervisor_only_cgroup_is_starting_until_a_runtime_was_seen() {
    let ids = host_ids();
    let supervisor_only = snapshot_of(vec![live(42644, 1, digest(2))]);
    let with_init = snapshot_of(vec![live(42644, 1, digest(2)), live(42673, 42644, digest(2))]);
    // Starting: "not yet" (bounded by the conversation loop), never a refusal.
    let mut census = Census::default();
    for snapshot in [&supervisor_only, &supervisor_only, &with_init] {
        assert_eq!(census.record(snapshot, &ids, &limits(), false, false), Turn::Classified);
    }
    assert!(!census.runtime_seen);
    assert_eq!(census.finish(&ids), None);
    // Once the runtime was seen, the run ending before the conversation completed is refused.
    assert_eq!(census.record(&snapshot_of(host_members()), &ids, &limits(), false, false), Turn::Classified);
    assert_eq!(census.record(&supervisor_only, &ids, &limits(), false, false), Turn::Failed);
    assert_eq!((census.finish(&ids), census.site), (Some(incomplete::RUNTIME_GONE), Some(site::RUNTIME_GONE_EARLY)));
    assert!(incomplete_message(incomplete::RUNTIME_GONE).contains("runtime_gone"));
    // ... but not after it completed (the tear-down).
    let mut census = Census::default();
    census.record(&snapshot_of(host_members()), &ids, &limits(), false, false);
    assert_eq!(census.record(&supervisor_only, &ids, &limits(), false, true), Turn::Classified);
    assert_eq!(census.finish(&ids), None);
    // The bridge's pre-exec fork (a second runtime digest, two runtime-digest
    // members) is still a running runtime; it then becomes the bridge.
    let mut census = Census::default();
    let runtime_only = snapshot_of(host_members()[..4].to_vec());
    let pre_exec = snapshot_of([host_members()[..4].to_vec(), vec![live(42677, 42675, digest(3))]].concat());
    for snapshot in [&runtime_only, &pre_exec, &snapshot_of(host_members())] {
        assert_eq!(census.record(snapshot, &ids, &limits(), false, false), Turn::Classified);
    }
    assert_eq!(census.finish(&ids), None);
}

/// The regression: at the end of the run (after `tools/list` was answered,
/// the worker exited 125) the supervisor is in its exit path, no longer listed
/// in `cgroup.procs`, while its pidfd is not readable yet. The harness used to
/// classify that agreeing snapshot at once and refuse `launcher_missing`.
#[test]
fn an_exiting_main_process_is_awaited_not_refused() {
    let ids = host_ids();
    let exiting = snapshot_of(Vec::new());
    // What the harness waits for: the main process's exit, nothing else.
    assert_eq!(absent_parents(&exiting, &ids), [HOST_SUPERVISOR]);
    // Old behaviour: classified at once (no confirmation yet) -> refused.
    assert_eq!(classify_sited(&exiting, &ids, &limits()).err(), Some((incomplete::LAUNCHER_MISSING, site::LAUNCHER_ABSENT)));
    let mut census = Census::default();
    census.record(&snapshot_of(host_members()), &ids, &limits(), false, false);
    assert_eq!(census.record(&exiting, &ids, &limits(), false, true), Turn::Failed);
    assert_eq!((census.incomplete, census.site), (Some(incomplete::LAUNCHER_MISSING), Some(site::LAUNCHER_ABSENT)));
    // New: the harness waits for the pidfd; once it confirms the exit after
    // the conversation, the turn is the tear-down and not classified.
    let mut census = Census::default();
    census.record(&snapshot_of(host_members()), &ids, &limits(), false, false);
    assert_eq!(census.record(&exiting, &ids, &limits(), true, true), Turn::Ended);
    assert_eq!(census.finish(&ids), None);
    // A confirmed exit before the conversation completed is still refused, at its own site.
    let mut census = Census::default();
    assert_eq!(census.record(&snapshot_of(host_members()), &ids, &limits(), true, false), Turn::Failed);
    assert_eq!((census.incomplete, census.site), (Some(incomplete::LAUNCHER_MISSING), Some(site::LAUNCHER_EXITED_EARLY)));
    // A pinned main process still listed is never "absent".
    assert!(absent_parents(&snapshot_of(host_members()), &ids).is_empty());
    let replaced = snapshot_of(vec![Member { pid: HOST_SUPERVISOR, read: MemberRead::Live { start: 1, ppid: 1, exe: digest(2) } }]);
    assert!(absent_parents(&replaced, &ids).is_empty());
    assert_eq!(classify_sited(&replaced, &ids, &limits()).err(), Some((incomplete::UNEXPECTED_PROCESS, site::LAUNCHER_REPLACED)));
}

#[test]
fn an_exiting_init_is_awaited_and_its_children_are_still_witnessed() {
    // The PID-namespace tear-down: init (42673) is exiting and unlisted while
    // the forwarder and the re-parented bridge are still listed.
    let ids = host_ids();
    let zapping = snapshot_of(vec![live(42644, 1, digest(2)), live(42674, 42673, digest(5)), live(42677, 42673, digest(4))]);
    assert_eq!(absent_parents(&zapping, &ids), [42673]);
    assert_eq!(classify_sited(&zapping, &ids, &limits()).err(), Some((incomplete::UNEXPECTED_PROCESS, site::PARENT_ABSENT)));
    // Init's exit confirmed: the snapshot is witnessed (documented digests,
    // known sequences) and taken again; the retake classifies.
    let mut census = Census::default();
    census.record(&snapshot_of(host_members()), &ids, &limits(), false, false);
    assert_eq!(census.witness(&zapping, &ids, &limits()), Turn::Classified);
    assert_eq!(census.record(&snapshot_of(vec![live(42644, 1, digest(2))]), &ids, &limits(), false, true), Turn::Classified);
    assert_eq!((census.finish(&ids), census.bridges(&ids)), (None, 1));
    // A process seen only in a witnessed snapshot is still counted and checked.
    let mut census = Census::default();
    census.record(&snapshot_of(host_members()), &ids, &limits(), false, false);
    let second_bridge = snapshot_of(vec![live(42644, 1, digest(2)), live(42690, 42673, digest(4))]);
    assert_eq!(census.witness(&second_bridge, &ids, &limits()), Turn::Classified);
    assert_eq!(census.bridges(&ids), 2);
    let unknown = snapshot_of(vec![live(42644, 1, digest(2)), live(42691, 42673, digest(9))]);
    assert_eq!(census.witness(&unknown, &ids, &limits()), Turn::Failed);
    assert_eq!((census.incomplete, census.site), (Some(incomplete::UNEXPECTED_PROCESS), Some(site::EXEC_SEQUENCE)));
    // Member-level gaps refuse a witnessed snapshot as well.
    for (read, reason) in [(MemberRead::Exited { confirmed: false }, incomplete::UNCONFIRMED_EXIT),
        (MemberRead::Unreadable, incomplete::UNREADABLE_PROCESS), (MemberRead::HashError, incomplete::HASH_ERROR)] {
        let mut census = Census::default();
        let gap = snapshot_of(vec![live(42644, 1, digest(2)), live(42674, 42673, digest(5)), Member { pid: 42675, read }]);
        assert_eq!(census.witness(&gap, &ids, &limits()), Turn::Failed);
        assert_eq!(census.incomplete, Some(reason));
    }
}

#[test]
fn the_first_failure_is_kept_with_its_site() {
    let ids = host_ids();
    let mut census = Census::default();
    let unknown = snapshot_of([host_members(), vec![live(42699, 42675, digest(9))]].concat());
    assert_eq!(census.record(&unknown, &ids, &limits(), false, false), Turn::Failed);
    census.fail(incomplete::PROCESS_FACTS, site::PROCESS_FACTS);
    assert_eq!(census.record(&snapshot_of(Vec::new()), &ids, &limits(), false, true), Turn::AlreadyFailed);
    assert_eq!((census.finish(&ids), census.site), (Some(incomplete::UNEXPECTED_PROCESS), Some(site::MISPLACED)));
    // The diagnosis keeps the last good turn and the FIRST failing one.
    let mut diag = CensusDiag::default();
    let turn = |at_ms: u64| SnapshotDiag { at_ms, ..SnapshotDiag::default() };
    diag.note(Turn::Classified, || turn(1));
    diag.note(Turn::Failed, || turn(2));
    diag.note(Turn::Failed, || turn(3));
    diag.note(Turn::AlreadyFailed, || turn(4));
    diag.note(Turn::Ended, || turn(5));
    assert_eq!((diag.last_good.map(|s| s.at_ms), diag.first_failure.map(|s| s.at_ms)), (Some(1), Some(2)));
}

#[test]
fn member_read_failures_and_roles_are_named_for_the_diagnosis() {
    let ids = host_ids();
    let snapshot = snapshot_of(vec![live(42644, 1, digest(2)), live(42673, 42644, digest(2)), live(42680, 42673, digest(2)),
        live(42675, 42673, digest(3)), live(42681, 42675, digest(3)), live(42682, 42675, digest(9)),
        Member { pid: 42683, read: MemberRead::Exited { confirmed: true } }, Member { pid: 42684, read: MemberRead::Exited { confirmed: false } },
        Member { pid: 42685, read: MemberRead::Unreadable }, Member { pid: 42686, read: MemberRead::HashError }]);
    let notes: BTreeMap<u32, MemberNote> = [(42685, MemberNote { uid: None, errno: Some(libc::EACCES) }),
        (42686, MemberNote { uid: None, errno: Some(libc::EIO) })].into_iter().collect();
    assert_eq!(member_lines(&snapshot, &notes, &ids), [
        "42644 1 ? supervisor 02020202".to_owned(), "42673 42644 ? init 02020202".to_owned(),
        "42680 42673 ? pre-exec 02020202".to_owned(), "42675 42673 ? runtime 03030303".to_owned(),
        "42681 42675 ? pre-exec 03030303".to_owned(), "42682 42675 ? unknown 09090909".to_owned(),
        "42683 - - exited_confirmed".to_owned(), "42684 - - exited_unconfirmed".to_owned(),
        format!("42685 - - unreadable({})", libc::EACCES), format!("42686 - - hash_error({})", libc::EIO)]);
    let mut responder = Responder::default();
    responder.feed(&line(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))).unwrap();
    assert_eq!(responder.counts(), (1, 0));
    responder.release_initialize(true).unwrap();
    assert_eq!(responder.counts(), (1, 1));
}

#[test]
fn diagnosis_names_the_incomplete_site_and_the_census_turns_within_40_lines() {
    let ids = host_ids();
    let good = snapshot_of(host_members());
    let many = snapshot_of((0..100u32).map(|index| live(50000 + index, 1, digest(9))).collect());
    let d = Diagnosis { incomplete: Some(incomplete::LAUNCHER_MISSING), census: CensusDiag {
        launcher: Some((HOST_SUPERVISOR, 777)), site: Some(site::LAUNCHER_ABSENT),
        last_good: Some(SnapshotDiag { at_ms: 5100, gap: None, members: member_lines(&good, &host_notes(988), &ids),
            launcher_exited: false, received: 3, sent: 2, after_conversation: true }),
        first_failure: Some(SnapshotDiag { at_ms: 5230, gap: None, members: member_lines(&many, &BTreeMap::new(), &ids),
            launcher_exited: false, received: 3, sent: 2, after_conversation: true }) },
        ..Diagnosis::default() };
    let lines = diagnosis_lines(&d);
    for expected in [
        "incomplete: launcher_missing",
        "census: pinned main process pid 42644 start 777",
        "incomplete site: classify.launcher_absent",
        "census first failing snapshot: +5230 ms, gap none, main pidfd exited false, frames received 3 sent 2, \
            after_conversation true, 100 members",
        "census first failing snapshot| ... 84 more",
        "census last good snapshot: +5100 ms, gap none, main pidfd exited false, frames received 3 sent 2, \
            after_conversation true, 5 members",
        "census last good snapshot| 42644 1 988 supervisor 02020202",
        "census last good snapshot| 42677 42675 76202 bridge 04040404",
    ] {
        assert!(lines.contains(&format!("{DIAG_PREFIX}{expected}")), "{expected}: {lines:?}");
    }
    let census_block = lines.iter().filter(|line| line.starts_with(&format!("{DIAG_PREFIX}census"))
        || line.starts_with(&format!("{DIAG_PREFIX}incomplete site"))).count();
    assert!(census_block <= 40, "{census_block}");
    // Nothing of it without an incomplete observation.
    let quiet = diagnosis_lines(&Diagnosis { census: d.census.clone(), ..Diagnosis::default() });
    assert!(!quiet.iter().any(|line| line.contains("census")));
}

// ── F3: the tear-down signals only the run's own processes ──────────────

#[test]
fn teardown_kills_only_members_of_the_run_cgroups() {
    let slice = "duduclaw-employees-ab.slice";
    let unit = "/duduclaw.slice/duduclaw-employees.slice/duduclaw-employees-ab.slice/duduclaw-run-cd.service";
    let groups = run_cgroups(unit, slice);
    assert_eq!(groups, [unit.to_owned(), "/duduclaw.slice/duduclaw-employees.slice/duduclaw-employees-ab.slice".to_owned()]);
    assert_eq!(run_cgroups("/system.slice/other.service", slice), ["/system.slice/other.service".to_owned()]);
    let refs: Vec<&str> = groups.iter().map(String::as_str).collect();
    let process = |pid: u32, cgroup: Option<&str>| UidProcess { pid, start: start_of(pid), cgroup: cgroup.map(str::to_owned) };
    let below_unit = format!("{unit}/runtime");
    let processes = [
        process(20, Some(unit)),
        process(21, Some(below_unit.as_str())),
        process(22, Some("/duduclaw.slice/duduclaw-employees.slice/duduclaw-employees-ab.slice")),
        // Same uid elsewhere: never signalled.
        process(30, Some("/user.slice/user-1000.slice/session-1.scope")),
        process(31, Some("/duduclaw.slice/duduclaw-employees.slice/duduclaw-employees-abc.slice")),
        process(32, Some("/duduclaw.slice/duduclaw-employees.slice")),
        process(33, None),
        process(1, Some(unit)),
    ];
    let selection = teardown_selection(&processes, &refs);
    assert_eq!(selection.kill, [(20, start_of(20)), (21, start_of(21)), (22, start_of(22))]);
    assert_eq!(selection.foreign, [(30, start_of(30)), (31, start_of(31)), (32, start_of(32)), (33, start_of(33)), (1, start_of(1))]);
    assert!(selection.foreign.iter().all(|foreign| !selection.kill.contains(foreign)));
    // No run cgroup known (the unit never started): nothing is ever signalled.
    let none = teardown_selection(&processes, &[]);
    assert!(none.kill.is_empty() && none.foreign.len() == processes.len());
    assert_eq!(FOREIGN_UID_PROCESS, "foreign_uid_process");
}

// ── F5: the harness directory is the guard's from its mkdir on ──────────

#[test]
fn harness_directory_is_registered_before_it_is_secured() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("duduclaw-acceptance-test");
    let mut registered: Option<PathBuf> = None;
    let result = make_registered_dir(&path, 0o700, |created| registered = Some(created.to_path_buf()),
        |_| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    assert!(result.is_err());
    assert_eq!(registered.as_deref(), Some(path.as_path()), "a failing chmod/chown leaves a registered directory");
    assert!(path.is_dir());
    // A failed mkdir registers nothing.
    let mut again: Option<PathBuf> = None;
    assert!(make_registered_dir(&path, 0o700, |created| again = Some(created.to_path_buf()), |_| Ok(())).is_err());
    assert!(again.is_none());
    // The success path runs the securing step on the registered directory.
    let other = dir.path().join("second");
    let mut secured = false;
    assert!(make_registered_dir(&other, 0o700, |_| {}, |created| { secured = created == other.as_path(); Ok(()) }).is_ok());
    assert!(secured);
}

#[test]
fn out_source_adds_only_the_record_and_passes_the_production_checks() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let raw = fs::read(&request.source).unwrap();
    let prepared = prepare(&request, &raw).unwrap();
    let (_, _, record) = evidence(&prepared);
    // Another directory: relative syscall lists become absolute (the same files).
    let out = accepted_source(&raw, &request.source, &request.out_source, LABEL, &record).unwrap();
    let parsed: PolicySource = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed.runtimes.len(), 2);
    assert!(parsed.runtimes[0].mcp_acceptance.is_some() && parsed.runtimes[1].mcp_acceptance.is_none());
    let value: Value = serde_json::from_slice(&out).unwrap();
    let directory = fs::canonicalize(fixture.dir.path().join("source")).unwrap();
    assert_eq!(value["forwarder_seccomp"]["x86_64"], json!(directory.join("x86_64.syscalls").to_str().unwrap()));
    assert_eq!(value["runtimes"][1]["native"]["seccomp"]["aarch64"], json!(directory.join("aarch64.syscalls").to_str().unwrap()));
    let mut expected: Value = serde_json::from_slice(&raw).unwrap();
    for pointer in ["/forwarder_seccomp", "/runtimes/0/native/seccomp", "/runtimes/1/native/seccomp"] {
        let object = expected.pointer_mut(pointer).unwrap();
        for arch in ["x86_64", "aarch64"] {
            let name = object[arch].as_str().unwrap().to_owned();
            object[arch] = json!(directory.join(name).to_str().unwrap());
        }
    }
    expected["runtimes"][0]["mcp_acceptance"] = serde_json::to_value(&record).unwrap();
    assert_eq!(value, expected, "nothing else changes");
    let (generated, policy) = build_policy_bytes(&out, &request.out_source, &request.artifacts, Arch::X86_64, None).unwrap();
    assert!(only_record_added(&prepared.generated, &generated.bytes, prepared.index, &record).is_ok());
    assert!(policy.runtimes[prepared.index].mcp_acceptance.is_some());
    assert!(policy.runtimes.iter().filter(|r| r.mcp_acceptance.is_some()).count() == 1);
    assert!(only_record_added(&prepared.generated, &prepared.generated, prepared.index, &record).is_err());
    // The same directory: paths stay relative.
    let beside = fixture.dir.path().join("source").join("accepted.json");
    let same = accepted_source(&raw, &request.source, &beside, LABEL, &record).unwrap();
    let value: Value = serde_json::from_slice(&same).unwrap();
    assert_eq!(value["forwarder_seccomp"]["x86_64"], json!("x86_64.syscalls"));
    assert!(build_policy_bytes(&same, &beside, &request.artifacts, Arch::X86_64, None).is_ok());
    // A record never lands on another runtime, nor twice.
    assert!(accepted_source(&raw, &request.source, &beside, "no-such-runtime", &record).is_err());
    assert!(accepted_source(&same, &beside, &beside, LABEL, &record).is_err());
    // The input file is never modified.
    assert_eq!(fs::read(&request.source).unwrap(), raw);
}

#[test]
fn prepare_refuses_artifacts_that_differ_from_the_record() {
    let fixture = Fixture::new();
    let raw = fs::read(fixture.source_path()).unwrap();
    assert!(prepare(&fixture.request(), &raw).is_ok());
    fs::write(fixture.artifacts().join("duduclaw-mcp-bridge"), "\u{7f}ELF another bridge").unwrap();
    // The scanned manifest follows the file, so the bridge digest changes with it.
    let prepared = prepare(&fixture.request(), &raw).unwrap();
    assert_eq!(prepared.bridge_digest, sha("\u{7f}ELF another bridge".as_bytes()));
    let mut request = fixture.request();
    request.label = "api-worker-second".into();
    let second = prepare(&request, &raw).unwrap();
    assert!(second.native().forwarder.is_none());
    request.label = "no-such-runtime".into();
    assert!(prepare(&request, &raw).is_err());
}

// ── P2: the employee image is a left-over only while an object exists ───

#[test]
fn employee_image_is_left_over_only_while_one_of_its_objects_exists() {
    use Presence::*;
    let gone = ImageObjects { file: Absent, loop_device: Absent, mount: Absent };
    // The first real-host run: destroy returned an error (its operation id was
    // the provision's), yet nothing was left on the host. Nothing is reported.
    assert!(image_leftovers(&gone).is_empty());
    assert_eq!(image_leftovers(&ImageObjects { file: Present, ..gone }), [LEFT_IMAGE_FILE]);
    assert_eq!(image_leftovers(&ImageObjects { loop_device: Present, ..gone }), [LEFT_IMAGE_LOOP]);
    assert_eq!(image_leftovers(&ImageObjects { mount: Present, ..gone }), [LEFT_IMAGE_MOUNT]);
    // A place that cannot be read never proves absence.
    assert_eq!(image_leftovers(&ImageObjects { file: Unknown, loop_device: Unknown, mount: Unknown }),
        [LEFT_IMAGE_FILE, LEFT_IMAGE_LOOP, LEFT_IMAGE_MOUNT]);
    // H's storage directory: only its bookkeeping files may remain.
    assert_eq!(storage_entries_presence(&[]), Absent);
    assert_eq!(storage_entries_presence(&["storage.lock".into(), "state.json".into(), "state.new".into()]), Absent);
    assert_eq!(storage_entries_presence(&["state.json".into(), format!("{}.img", "ab".repeat(16))]), Present);
    assert_eq!(storage_entries_presence(&[format!("{}.mnt", "ab".repeat(16))]), Present);
    assert_eq!(storage_entries_presence(&["anything-else".into()]), Present);
    // A loop device backed by a file below the storage directory, also once unlinked.
    let storage = Path::new("/var/lib/duduclaw-acceptance-0123/storage");
    let image = format!("{}/{}.img", storage.display(), "ab".repeat(16));
    assert!(backing_below(&format!("{image}\n"), storage));
    assert!(backing_below(&format!("{image} (deleted)\n"), storage));
    assert!(!backing_below("/var/lib/snapd/snaps/core22_1380.snap\n", storage));
    assert!(!backing_below("/var/lib/duduclaw-acceptance-0123/storage2/x.img\n", storage));
    assert!(!backing_below("\n", storage));
    // A mount below the storage directory in the harness's own mountinfo.
    let table = "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
        90 22 7:3 / /var/lib/duduclaw-acceptance-0123/storage/abab.mnt rw,nosuid,nodev,noexec - ext4 /dev/loop3 rw,errors=remount-ro\n";
    assert!(mountinfo_below(table, storage));
    assert!(!mountinfo_below(table.lines().next().unwrap(), storage));
    assert!(mountinfo_below("91 22 7:4 / /var/lib/with\\040space/x rw - ext4 /dev/loop4 rw", Path::new("/var/lib/with space")));
    assert!(!mountinfo_below("92 22 7:5 / /var/lib/duduclaw-acceptance-01234 rw - ext4 /dev/loop5 rw",
        Path::new("/var/lib/duduclaw-acceptance-0123")));
}

// ── A: the failure diagnosis ────────────────────────────────────────────

#[test]
fn diagnosis_phase_names_are_fixed() {
    let phases = [(HarnessPhase::Prepare, "prepare"), (HarnessPhase::Storage, "storage"), (HarnessPhase::Projection, "projection"),
        (HarnessPhase::Seal, "seal"), (HarnessPhase::StartUnit, "start_unit"), (HarnessPhase::BindUnit, "bind_unit"),
        (HarnessPhase::AwaitInitialize, "await_initialize"), (HarnessPhase::Observe, "observe"),
        (HarnessPhase::AwaitToolsList, "await_tools_list"), (HarnessPhase::AwaitExit, "await_exit"),
        (HarnessPhase::Teardown, "teardown")];
    assert_eq!(HarnessPhase::default(), HarnessPhase::Prepare);
    for (phase, name) in phases {
        assert_eq!(phase.name(), name);
        let diagnosis = Diagnosis { phase, ..Diagnosis::default() };
        assert_eq!(diagnosis_lines(&diagnosis)[0], format!("{DIAG_PREFIX}phase {name}"));
    }
    // Nothing collected: every fact says so, no stream line.
    let lines = diagnosis_lines(&Diagnosis::default());
    for expected in ["harness error: none recorded", "unit: not read (never started)",
        "trampoline status: no error record received", "stdout: last 0 of 0 bytes", "stderr: last 0 of 0 bytes"] {
        assert!(lines.contains(&format!("{DIAG_PREFIX}{expected}")), "{expected}: {lines:?}");
    }
    assert!(!lines.iter().any(|line| line.contains("| ") || line.contains("run ") || line.contains("employee ")));
}

#[test]
fn diagnosis_trampoline_error_names_are_the_status_codes() {
    use duduclaw_run::Error as E;
    let all = [E::UnsupportedPlatform, E::UnsupportedKernel, E::RegistryUnavailable, E::Bootstrap, E::Descriptors,
        E::DescriptorLimit, E::Executable, E::Mount, E::Namespace, E::Privileges, E::Landlock, E::Seccomp, E::State,
        E::Process, E::Status, E::Exec];
    for (index, error) in all.into_iter().enumerate() {
        let code = u8::try_from(index + 1).unwrap();
        assert_eq!(Some(trampoline_error_name(code)), serde_json::to_value(error).unwrap().as_str());
        #[cfg(target_os = "linux")]
        assert_eq!(duduclaw_run::linux::bootstrap::error_status(error)[5], code);
    }
    assert_eq!(trampoline_error_name(0), "unknown");
    assert_eq!(trampoline_error_name(17), "unknown");
}

#[test]
fn diagnosis_tail_keeps_the_last_bytes() {
    let mut tail = Tail::new(4);
    tail.push(b"abcdef");
    assert_eq!(tail.bytes(), b"cdef");
    tail.push(b"gh");
    assert_eq!(tail.bytes(), b"efgh");
    tail.push(b"");
    assert_eq!(tail.bytes(), b"efgh");
    assert_eq!(Diagnosis::default().stderr.bytes().len(), 0);
}

#[test]
fn diagnosis_is_bounded_printable_and_never_carries_a_provider_key() {
    // The provider key's shape (`providers_document`): 32 lowercase hex digits.
    let key = "0123456789abcdef0123456789abcdef";
    let mut d = Diagnosis { run: Some([0xab; 16]), employee: Some([0xcd; 16]), phase: HarnessPhase::BindUnit,
        harness_error: Some(ErrorFact::io("transient run unit", &io::Error::from_raw_os_error(libc::EACCES))),
        unit: UnitReadback::Read(UnitResult { active_state: "failed".into(), result: "exit-code".into(), exec_main_code: 1,
            exec_main_status: 125 }),
        status_error: Some(4), run_end: Some(format!("exited 125\n{key}")), incomplete: Some(incomplete::LAUNCHER_MISSING),
        teardown: vec![ErrorFact::code("storage destroy", ErrorCode::InvalidRequest), ErrorFact::timeout("run cgroup removal"),
            ErrorFact::fixed("a mount below the harness directory")],
        ..Diagnosis::default() };
    let mut stderr = vec![b'x'; 3 * STDERR_TAIL_BYTES];
    stderr.extend_from_slice(b"\n\x1b[31mred\r\tbell\x07\n");
    stderr.extend_from_slice(format!("{{\"api_key\":\"{key}\"}}\n").as_bytes());
    stderr.extend_from_slice(&[0xff, 0xfe, b'\n']);
    d.stderr.push(&stderr);
    d.stdout.push(format!("key={}\n", key.to_uppercase()).as_bytes());
    let lines = diagnosis_lines(&d);
    assert!(lines.iter().all(|line| line.starts_with(DIAG_PREFIX)), "{lines:?}");
    let text = lines.join("\n");
    assert!(!text.to_lowercase().contains(key), "the key never appears");
    for expected in [
        "phase bind_unit".to_owned(),
        format!("run {}", "ab".repeat(16)),
        format!("employee {}", "cd".repeat(16)),
        format!("harness error: transient run unit (kind PermissionDenied, errno {})", libc::EACCES),
        "unit: ActiveState failed Result exit-code ExecMainCode 1 (exited) ExecMainStatus 125".to_owned(),
        "trampoline status: error bootstrap (code 4)".to_owned(),
        format!("run end: exited 125?{}", "*".repeat(32)),
        "incomplete: launcher_missing".to_owned(),
        "teardown step failed: storage destroy (code InvalidRequest)".to_owned(),
        "teardown step failed: run cgroup removal (kind TimedOut, errno none)".to_owned(),
        "teardown step failed: a mount below the harness directory".to_owned(),
        format!("stderr: last {STDERR_TAIL_BYTES} of {} bytes", stderr.len()),
        "stderr| ?[31mred??bell?".to_owned(),
        format!("stderr| {{\"api_key\":\"{}\"}}", "*".repeat(32)),
        "stderr| \u{fffd}\u{fffd}".to_owned(),
        format!("stdout| key={}", "*".repeat(32)),
    ] {
        assert!(lines.contains(&format!("{DIAG_PREFIX}{expected}")), "{expected}: {lines:?}");
    }
    // No control character survives (every `\n` became a line break).
    assert!(lines.iter().all(|line| !line.chars().any(char::is_control)));
    // Bounded: at most the last 8 KiB of stderr and 2 KiB of stdout, as characters.
    for (name, bound) in [("stderr", STDERR_TAIL_BYTES), ("stdout", STDOUT_TAIL_BYTES)] {
        let prefix = format!("{DIAG_PREFIX}{name}| ");
        let shown: Vec<&str> = lines.iter().filter_map(|line| line.strip_prefix(prefix.as_str())).collect();
        assert!(!shown.is_empty());
        assert!(shown.iter().map(|line| line.chars().count() + 1).sum::<usize>() <= bound + 1, "{name}");
    }
    // A short hex value (an id, an errno) is printed as is.
    assert_eq!(printable(b"abc123\x00", 64), "abc123?");
}

// ── the real launch (root, disposable host) ─────────────────────────────

#[test]
#[ignore = "iso_root_: disposable root Linux VM WITHOUT any isolation installation or protected-mode marker \
    (DUDUCLAW_ISO_DISPOSABLE=1), systemd, /usr/sbin/mkfs.ext4; env DUDUCLAW_ISO_POLICY_TOOL (a built \
    duduclaw-isolation-policy, feature policy-tool), DUDUCLAW_ISO_ACCEPT_SOURCE, DUDUCLAW_ISO_ACCEPT_ARTIFACTS \
    (stage_policy_inputs output), DUDUCLAW_ISO_ACCEPT_TRAMPOLINE (the matching duduclaw-run), DUDUCLAW_ISO_ACCEPT_LABEL; \
    launches the real api worker and bridge once; --test-threads=1"]
fn iso_root_policy_accept_api_worker_end_to_end() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"));
    let env = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("{key} is required"));
    let (tool, source, artifacts, trampoline, label) = (env("DUDUCLAW_ISO_POLICY_TOOL"), env("DUDUCLAW_ISO_ACCEPT_SOURCE"),
        env("DUDUCLAW_ISO_ACCEPT_ARTIFACTS"), env("DUDUCLAW_ISO_ACCEPT_TRAMPOLINE"), env("DUDUCLAW_ISO_ACCEPT_LABEL"));
    let native = Arch::native().unwrap();
    let arch = arch_name(native);
    let work = tempfile::tempdir().unwrap();
    let evidence = work.path().join("evidence");
    let out = work.path().join("accepted.json");
    // F1: the trampoline's digest, as the bundle script computes it from the staged snapshot.
    let (trampoline_digest, trampoline_size) = file_digest(Path::new(&trampoline)).unwrap();
    let trampoline_hex = hex(&trampoline_digest.0);
    // The harness needs its own single-threaded process (private mount namespace).
    let status = std::process::Command::new(&tool)
        .args(["accept", "--source", source.as_str(), "--artifacts-dir", artifacts.as_str(), "--arch", arch,
            "--label", label.as_str(), "--trampoline", trampoline.as_str(), "--trampoline-sha256", trampoline_hex.as_str(),
            "--evidence-dir", evidence.to_str().unwrap(), "--out-source", out.to_str().unwrap()])
        .env_clear().env("DUDUCLAW_ISO_DISPOSABLE", "1").status().expect("run the policy tool");
    assert!(status.success(), "accept failed: {status}");
    assert!(evidence.join(MANIFEST_FILE).is_file() && evidence.join(TRANSCRIPT_FILE).is_file());
    let policy = work.path().join("policy.json");
    let strings = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
    let built = super::super::run(&strings(&["build", "--source", out.to_str().unwrap(), "--artifacts-dir", artifacts.as_str(),
        "--arch", arch, "--out", policy.to_str().unwrap()]));
    assert!(built.is_ok(), "{built:?}");
    // A release manifest for these artifacts, this policy and the trampoline
    // (what the packager signs; `verify --evidence-dir` requires one).
    let policy_bytes = fs::read(&policy).unwrap();
    let mut release = load_manifest(Path::new(&artifacts), None, native, false).unwrap();
    release.artifacts.push(ReleaseArtifact { id: "duduclaw-run".into(), sha256: trampoline_digest, size_bytes: trampoline_size,
        mode: 0o755, install_target: InstallTarget::Trampoline });
    release.artifacts.push(ReleaseArtifact { id: "isolation-policy-v1".into(), sha256: sha(&policy_bytes),
        size_bytes: policy_bytes.len() as u64, mode: 0o444, install_target: InstallTarget::RuntimeEntry { registry_id: POLICY_ID.into() } });
    let release_path = work.path().join("release-manifest.json");
    fs::write(&release_path, serde_json::to_vec(&release).unwrap()).unwrap();
    let harness_hex = hex(&file_digest(Path::new(&tool)).unwrap().0 .0);
    let verified = super::super::run(&strings(&["verify", "--policy", policy.to_str().unwrap(), "--artifacts-dir", artifacts.as_str(),
        "--manifest", release_path.to_str().unwrap(), "--evidence-dir", evidence.to_str().unwrap(),
        "--harness-sha256", harness_hex.as_str()]));
    assert!(verified.as_ref().is_ok_and(|text| text.contains("1 acceptance record(s)")), "{verified:?}");
    assert!(!installed_under(Path::new("/")), "the harness must leave no installation fact behind");
}
