//! R8r-4 acceptance harness: gates, the gateway-end responder, the decision,
//! the evidence encoding and `verify --evidence-dir`, the output source. All
//! unprivileged; the real launch is `iso_root_policy_accept_api_worker_end_to_end`.
use super::*;
use serde_json::json;

const API_WORKER: &str = "\u{7f}ELF TEST api worker bytes";
const FORWARDER: &str = "\u{7f}ELF TEST egress forwarder bytes";
const BRIDGE: &str = "\u{7f}ELF TEST mcp bridge bytes";
const LABEL: &str = "api-worker-test";
const UID: u32 = 71_234;

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
        fs::write(dir.path().join("duduclaw-run"), "\u{7f}ELF TEST trampoline").unwrap();
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
            trampoline: self.dir.path().join("duduclaw-run"), evidence: self.evidence(), out_source: self.out_source() }
    }
    fn args(&self) -> Vec<String> {
        let request = self.request();
        ["--source", request.source.to_str().unwrap(), "--artifacts-dir", request.artifacts.to_str().unwrap(),
            "--arch", "x86_64", "--label", LABEL, "--trampoline", request.trampoline.to_str().unwrap(),
            "--evidence-dir", request.evidence.to_str().unwrap(), "--out-source", request.out_source.to_str().unwrap()]
            .iter().map(|value| (*value).to_owned()).collect()
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
        let path = root.path().join(fact.trim_start_matches('/'));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        assert!(installed_under(root.path()), "{fact}");
        fs::remove_file(&path).unwrap();
        assert!(!installed_under(root.path()));
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
        run_end: "exited 1".into() }
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
    ];
    for (name, mutate) in mutations {
        let mut observations = valid();
        mutate(&mut observations);
        assert!(decide(&observations).is_err(), "{name}");
    }
}

// ── evidence and the record ─────────────────────────────────────────────

fn evidence(prepared: &Prepared) -> (Vec<u8>, Vec<u8>, McpAcceptance) {
    let transcript = transcript_jsonl(&conversation());
    let observations = valid();
    let manifest = evidence_manifest(&EvidenceSummary { arch: Arch::X86_64, harness_sha256: digest(1), trampoline_sha256: digest(2),
        runtime_sha256: prepared.runtime_digest, bridge_sha256: prepared.bridge_digest,
        projection_sha256: prepared.projection.digest(), launch_policy_sha256: prepared.launch_policy_digest,
        transcript_sha256: sha(&transcript), observations: &observations, kernel: "6.8.0-test", finished_unix: 1 });
    let record = acceptance_record(prepared, &manifest);
    (transcript, manifest, record)
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
    let release = load_manifest(&request.artifacts, None, Arch::X86_64, false).unwrap();
    let evidence_dir = fixture.dir.path().join("evidence");
    let label_dir = evidence_dir.join(LABEL);
    fs::create_dir_all(&label_dir).unwrap();
    fs::write(label_dir.join(TRANSCRIPT_FILE), &transcript).unwrap();
    fs::write(label_dir.join(MANIFEST_FILE), &manifest).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir).unwrap().contains("1 acceptance record(s)"));
    // Through the command line, with and without the flag.
    let policy_path = fixture.dir.path().join("policy.json");
    fs::write(&policy_path, &generated.bytes).unwrap();
    let verify = |extra: &[&str]| {
        let mut args: Vec<String> = ["verify", "--policy", policy_path.to_str().unwrap(), "--artifacts-dir",
            request.artifacts.to_str().unwrap(), "--arch", "x86_64"].iter().map(|v| (*v).to_owned()).collect();
        args.extend(extra.iter().map(|v| (*v).to_owned()));
        super::super::run(&args)
    };
    assert!(verify(&[]).is_ok());
    assert!(verify(&["--evidence-dir", evidence_dir.to_str().unwrap()]).unwrap().contains("backed by harness evidence"));
    // No evidence at all.
    let empty = fixture.dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert_eq!(verify(&["--evidence-dir", empty.to_str().unwrap()]).err().map(|e| e.0), Some(1));
    // A tampered transcript.
    let mut tampered = transcript.clone();
    tampered[10] ^= 1;
    fs::write(label_dir.join(TRANSCRIPT_FILE), &tampered).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir).is_err());
    fs::write(label_dir.join(TRANSCRIPT_FILE), &transcript).unwrap();
    // A tampered manifest (no longer the record's transcript_digest).
    let mut tampered = manifest.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    fs::write(label_dir.join(MANIFEST_FILE), &tampered).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir).is_err());
    fs::write(label_dir.join(MANIFEST_FILE), &manifest).unwrap();
    assert!(verify_evidence(&policy, &release, Arch::X86_64, &evidence_dir).is_ok());
    // Artifacts whose digests differ from the record and the manifest.
    let accepted = policy.runtimes.iter().find(|r| r.mcp_acceptance.is_some()).unwrap();
    let acceptance = accepted.mcp_acceptance.as_ref().unwrap();
    assert!(check_evidence(&manifest, &transcript, acceptance, accepted, &release, Arch::X86_64).is_ok());
    let mut other = release.clone();
    for artifact in &mut other.artifacts {
        if artifact.id == "runtime-api-worker-v1" { artifact.sha256 = digest(9); }
    }
    assert!(check_evidence(&manifest, &transcript, acceptance, accepted, &other, Arch::X86_64).is_err());
    let mut other = release.clone();
    for artifact in &mut other.artifacts {
        if artifact.install_target == InstallTarget::McpBridge { artifact.sha256 = digest(9); }
    }
    assert!(check_evidence(&manifest, &transcript, acceptance, accepted, &other, Arch::X86_64).is_err());
    assert!(check_evidence(&manifest, &transcript, acceptance, accepted, &release, Arch::Aarch64).is_err());
    assert!(check_evidence(&manifest, b"", acceptance, accepted, &release, Arch::X86_64).is_err());
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
    let arch = arch_name(Arch::native().unwrap());
    let work = tempfile::tempdir().unwrap();
    let evidence = work.path().join("evidence");
    let out = work.path().join("accepted.json");
    // The harness needs its own single-threaded process (private mount namespace).
    let status = std::process::Command::new(&tool)
        .args(["accept", "--source", source.as_str(), "--artifacts-dir", artifacts.as_str(), "--arch", arch,
            "--label", label.as_str(), "--trampoline", trampoline.as_str(), "--evidence-dir", evidence.to_str().unwrap(),
            "--out-source", out.to_str().unwrap()])
        .env_clear().env("DUDUCLAW_ISO_DISPOSABLE", "1").status().expect("run the policy tool");
    assert!(status.success(), "accept failed: {status}");
    assert!(evidence.join(MANIFEST_FILE).is_file() && evidence.join(TRANSCRIPT_FILE).is_file());
    let policy = work.path().join("policy.json");
    let strings = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
    let built = super::super::run(&strings(&["build", "--source", out.to_str().unwrap(), "--artifacts-dir", artifacts.as_str(),
        "--arch", arch, "--out", policy.to_str().unwrap()]));
    assert!(built.is_ok(), "{built:?}");
    let verified = super::super::run(&strings(&["verify", "--policy", policy.to_str().unwrap(), "--artifacts-dir", artifacts.as_str(),
        "--arch", arch, "--evidence-dir", evidence.to_str().unwrap()]));
    assert!(verified.as_ref().is_ok_and(|text| text.contains("1 acceptance record(s)")), "{verified:?}");
    assert!(!installed_under(Path::new("/")), "the harness must leave no installation fact behind");
}
