# Native Linux isolation implementation

## Package E: disposable parse worker

Round 2 implementation, 2026-11-05. This section contains the former
`wiki/impl/native-linux-isolation-package-e-20261105.md` handoff and supersedes its
remaining-work list. No commit, installation, migration, deployment or live
protected-readiness acceptance was performed. The supplied worktree already
existed; this session did not create a worktree.

### Process and resource boundary

`duduclaw-parse-worker` is a workspace member. Its default library has no document
parser dependencies; `--features engine` builds the disposable executable. A/G
install the signed binary at `/usr/libexec/duduclaw/duduclaw-parse-worker` and the
socket/template supplied under `crates/duduclaw-parse-worker/systemd/`.
The root-owned system socket `/run/duduclaw-parse/parse.sock` is mode 0660, group
`duduclaw`; its parent is root-owned and not group-writable. The client checks
socket/parent ownership and root peer credentials. The gateway receives no
permission to submit arbitrary systemd properties or executable paths.

One socket-activated DynamicUser instance handles one seqpacket request. The
systemd instance name is an accepted-connection identifier, not request_id;
responses still bind request_id. The worker has PrivateNetwork, IPAddressDeny,
no capabilities, NNP, private IPC/devices, an empty read-only root with runtime
libraries projected, no application state and a bounded temporary filesystem.
It disables dumpability before receiving content, verifies its dynamic uid range,
then applies rlimits and a native x86_64/aarch64 syscall allowlist. Parsing cannot
open files, create/connect sockets, spawn processes, change namespaces or exec.

Limits: 512 MiB address space and cgroup memory, no swap, one task, one CPU,
64 fds, 30-second maximum request budget, 35-second service runtime, 5-second
startup and 1-second stop grace. Output file hard limit is 64 MiB; the process
narrows it to the actual request's output budget, or zero for no-output requests.
The socket caps active connections at four and has a backlog of four. Socket
buffers accommodate the 256 KiB frame. The process-local client admission is
acquired before allocating snapshots; connection admission precedes snapshotting.

A failed socket exchange applies a process-local cooldown of at most 41 seconds
from connection establishment, throttling repeated allocations. A socket can
still be in the accept backlog before service deadlines start, so cooldown expiry
is not proof of retirement, a synchronous cancellation receipt or a host-wide
reservation. Kernel queues and workers can retain a memfd until teardown. A/B must provide aggregate accounting
and verified retirement through the named launcher seam below.

### File protocol and output publication

A frame is the unchanged four-byte length plus bounded JSON, with ordered
SCM_RIGHTS descriptors. Inputs are authorized by the caller, copied into sealed
memfds and reopened read-only. Worker checks include fd count, size, regular
file type, access mode, tmpfs backing and seals. Output plans accept one empty,
unlinked, writable, sealable memfd, distinct from every input/output inode.
Truncated frames/ancillary data, unknown ancillary messages and mismatched fd
plans reject the request and close received descriptors.

For image/pack output, the worker drops all input and writable output descriptors
before replying. The client applies all four seals before reading size, SHA-256
and bytes. A retained writable mapping makes sealing fail; a retained ordinary fd
cannot mutate a sealed output. The hash is consistency evidence, not content
trust. Missing artifacts, mismatched digests, actual size over budget and incomplete
manifests are rejected. Pack destinations are revalidated by the caller and
published from a private staging directory, using Linux renameat2 NOREPLACE.
No ZIP parser or image decoder runs in the publishing gateway/CLI path.

### Implemented routes

| Surface | Behavior when isolation is required |
| --- | --- |
| Knowledge ingestion and direct extractor | PDF, DOCX, PPTX, XLSX/XLS, CSV, plain text, Markdown and HTML through worker; segment kind/label/page/sheet/line_rows/warnings preserved |
| Office preflight | Full caller DocumentLimits policy transmitted and checked in worker; public path and bytes entrypoints both route to it |
| Office artifact delivery | Magic/placeholder policy stays in caller; ZIP preflight runs in worker on the blocking pool |
| CSV and typed spreadsheet tools | Delimiter/header/offset/limit/sheet options, typed values, columns and truncation preserved; normal auditing/redaction remains in caller |
| Image dimensions, resize, screenshot masks | Bounded first-frame decoding; JPEG/PNG artifact bytes verified after sealing; no raw-image fallback on failure |
| CSS extraction and web search | Selector priority, element attributes, text/HTML/recursive JSON shapes; bounded results; no caller-side DOM parser |
| Expert ZIP import | Worker expands bytes and returns bounded entry manifest; caller validates names/ranges/collisions, stages and publishes without parsing ZIP |
| Legacy backup/portability import | Refused before archive parsing/staging; protected migration belongs to G's importer |
| Local ONNX/tokenizer, model bundle extraction and Kokoro model loading | Refused before parsing; existing Embedder/TtsProvider interfaces remain the A service integration points |
| Office-to-PDF preview | Legacy soffice spawn refused; A must provide isolated conversion execution |
| Signed updater archives | F owns these; untouched |

The worker engine shares the existing knowledge format-reader source through
`#[path]` with a local data-model facade, rather than depending on the knowledge
crate or duplicating its parser implementations. Expert ZIP filename decoding is
also shared, including GBK/Big5 and junk-entry reporting. The default client build
does not compile these sources.

Implementation ceilings narrow the contract: 64 MiB input/ZIP expansion (pack
extraction 50 MiB), 1,000 archive entries, depth 32, 8 MiB XML parts, 50,000 cells,
16,384-pixel image axes and 64 MiB decoded pixels. Per-text item remains 8 KiB,
aggregate text including metadata remains 128 KiB, frame remains 256 KiB.
A knowledge segment over 8 KiB is rejected, not silently split or truncated.
Complete XML scanning refuses a part/document beyond its configured scan budget;
it never treats an unchecked tail as safe. Table pagination remains explicitly
marked truncated. HTML bounds include recursion depth and node count. Basic
archive inspection rejects nested archives; pack extraction retains nested files
as bytes without recursively parsing them, preserving the importer's behavior.

### CONTRACT CHANGE

Additive changes in `duduclaw-isolation::worker`:

- Optional `ParseRequest.options`, omitted when absent; existing requests serialize
  with the same fields. Existing protocol version, canonical capability bytes,
  existing operation encodings and hard limits are unchanged.
- Operations for structured knowledge/table extraction, document policy preflight,
  image transforms, HTML extraction and pack expansion; closed bounded option
  enums and result types for those operations.
- `worker_media.rs` supplies bounded HTML query options.

Older workers reject new operation/option fields. There is no legacy parser
fallback. Matching version 1 alone does not prove support for the new adapters;
A/G must install the matching signed component set and run readiness probes.
Compatibility review of these additions is still required at integration.

### Named integration seams and remaining gates

- A/G call `client::require_isolation()` before admitting work, or set the
  root-owned gateway unit's `DUDUCLAW_ISOLATION_REQUIRED`. Any presence latches
  required mode, with no reset. The old protected-key mode does not automatically
  activate the new path. Install/verify signed binaries and units, and gate
  protected readiness on live probes; these are A/G responsibilities.
- A/B may install `Box<dyn client::WorkerLauncher>` once through
  `install_launcher`. `launch(ParseRequest, input_reservation_bytes)` returns a
  `WorkerSession`; its `exchange` accepts only the contract request and
  descriptors. The reservation is always the 64 MiB input ceiling because reader
  adapters discover the exact length during snapshotting; a zero initial input
  size is never a zero-byte reservation. The custom
  implementation owns host resource reservations and retains them through
  cancellation until unit/cgroup retirement is proven. Default is the fixed
  socket-activated launcher, never local execution. Results from an injected
  launcher are also bounded and validated by the client.
- D owns immutable request identity and authorized dirfd/openat2 resolution.
  E only consumes already-authorized files. Current legacy path adapters use
  NOFOLLOW/NONBLOCK plus regular-file checks; these are not substitutes for D's
  ancestor/mount/resource authorization.
- G replaces legacy backup/portability restore with the low-privilege importer.
  A supplies isolated model/inference/STT/TTS service execution and preconnected
  service fds. Parse operations LoadModel/LocalInference/TranscribeAudio/
  SynthesizeSpeech are explicitly unsupported by this document/media worker.
- Ubuntu 22.04, Ubuntu 24.04 and Debian 12 runtime acceptance, aarch64 runtime,
  gateway/CLI remote compilation, and Daybreak review remain unverified. The
  engineering subagent review is not a Daybreak approval. No full-product
  readiness, gateway load-test, cgroup OOM-killer or customer acceptance claim is
  made by passing small-crate tests or cross-compilation.

### Validation and remote commands

Normal developer checks (no gateway/CLI build on the shared Mac):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-parse-worker --features engine --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-parse-worker --all-targets --features engine --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-parse-worker --all-targets --features engine --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-parse-worker --no-default-features --offline
```

Remote builder (explicitly not run locally):

```sh
cargo check -p duduclaw-gateway -p duduclaw-cli
cargo test -p duduclaw-gateway parse_isolation_
cargo test -p duduclaw-cli parse_isolation_
cargo test -p duduclaw-knowledge parse_isolation_
cargo test -p duduclaw-gateway artifact_gate::tests
cargo test -p duduclaw-gateway computer_use_orchestrator::tests
cargo test -p duduclaw-parse-worker --features engine
```

Disposable root Linux VM, after installing the exact current binary and both
units (also requires the duduclaw group and cgroup v2):

```sh
cargo build -p duduclaw-parse-worker --features engine
install -D -m 0755 target/debug/duduclaw-parse-worker /usr/libexec/duduclaw/duduclaw-parse-worker
install -m 0644 crates/duduclaw-parse-worker/systemd/duduclaw-parse.socket /etc/systemd/system/
install -m 0644 crates/duduclaw-parse-worker/systemd/duduclaw-parse@.service /etc/systemd/system/
systemctl daemon-reload
systemctl start duduclaw-parse.socket
cargo test -p duduclaw-parse-worker --features engine -- --ignored iso_root_ --test-threads=1
```

The ignored tests cover text, malformed CFB, ZIP bomb, XML depth, oversized
results, real image/CSS and ZIP output, identity/capabilities/namespaces/hidden
paths/cgroup limits, disconnect and silent-client deadline cleanup, recovery,
and a disposable child exercising address-space allocation refusal, SSRF syscall
refusal and the production watchdog. The normal Linux suite covers SCM_RIGHTS,
fd cleanup/seals, output hashes, frames, deadlines and seccomp. Linux-specific
tests were only compiled on this Mac. Allocation refusal under RLIMIT_AS is not
a measured cgroup OOM-killer test; the systemd cleanup probe is not gateway
responsiveness under concurrent malicious load.

### Earlier round evidence retained

Round 1 recorded 19 macOS worker tests and 29 contract tests passing; Linux and
Windows all-target checks, client-only check and diff whitespace check passed.
It did not build gateway/CLI or run real systemd tests. Its missing Office/table/
image/CSS/archive adapters and parser-entry inventory are superseded above.
Earlier intermediate 15/18-test counts referred to older code. No new external
package version was added: the engine reuses locked zip, quick-xml, calamine,
pdf-extract, image, encoding_rs, scraper, ego-tree, csv, chrono and sha2; Linux uses
locked nix/libc. No new heavy model runtime dependency was introduced.


### Round 2 observed validation

| Command | Observed result on this Mac |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-parse-worker --features engine --offline` | 59 passed, 0 failed; Linux tests excluded by cfg |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline` | 32 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-parse-worker --all-targets --features engine --offline` | PASS; includes Linux transport, sandbox and seven ignored root tests |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-parse-worker --all-targets --features engine --offline` | PASS; invocation refuses on unsupported platforms |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-parse-worker --no-default-features --offline` | PASS; default client excludes document parser dependencies |
| `git diff --check` | PASS |

During implementation, the initial manifest used an absent workspace sha2 alias;
it was changed to the already-locked 0.10 version. The first Linux check ran before
the new archive module registration landed and failed with unresolved
`archive_adapter`; the complete tree passed subsequent checks. Earlier 49/58-test
results precede the workbook/archive/reservation tests; 59 is the final macOS worker count.
The root tests are present and compiled, not executed. Gateway/CLI/knowledge
consumer tests are written but not run locally under the delegated build limit.

Engineering review found and fixed the leftover public Office refusal and async
worker waits in screenshot/web callers. A subsequent review found no new
HIGH/CRITICAL in the reviewed diff. This is not a Daybreak security signoff.

### Full uncommitted package file inventory

This includes files carried over from round 1 as well as the round 2 completion.
The shared contract changes are explicitly listed above.

| File | Change |
| --- | --- |
| `CHANGELOG.md` | Unreleased user-facing entry. |
| `Cargo.lock` | Local worker package and edges to already-locked dependencies. |
| `Cargo.toml` | Register worker workspace member/dependency. |
| `crates/duduclaw-cli/Cargo.toml` | Client-only worker dependency. |
| `crates/duduclaw-cli/src/expert/safe_zip.rs` | Isolated expansion, manifest validation, private staging and no-replace publication. |
| `crates/duduclaw-cli/src/mcp/messaging.rs` | Bound search response size and move DOM parsing to worker/blocking pool. |
| `crates/duduclaw-cli/src/mcp/web.rs` | Run isolated CSS extraction on blocking pool. |
| `crates/duduclaw-cli/src/mcp_files.rs` | Typed CSV/spreadsheet worker adapter and row-shape tests. |
| `crates/duduclaw-cli/src/portability.rs` | Refuse legacy tar imports before parsing in required mode. |
| `crates/duduclaw-gateway/Cargo.toml` | Client-only worker dependency. |
| `crates/duduclaw-gateway/src/artifact_gate.rs` | Isolated Office container delivery preflight on blocking pool. |
| `crates/duduclaw-gateway/src/backup_restore.rs` | Refuse legacy archive restore before read/staging. |
| `crates/duduclaw-gateway/src/computer_use.rs` | Isolated mask adapter and async blocking-pool wrapper. |
| `crates/duduclaw-gateway/src/computer_use_orchestrator.rs` | Move screenshot mask pipeline to blocking pool. |
| `crates/duduclaw-gateway/src/computer_use_sessions/mod.rs` | Await masks and recheck live session. |
| `crates/duduclaw-gateway/src/computer_use_sessions/oversight.rs` | Recheck stop/authority after asynchronous masking. |
| `crates/duduclaw-gateway/src/computer_use_sessions/tests/oversight_cases.rs` | Stop-after-mask regression test. |
| `crates/duduclaw-gateway/src/document_limits.rs` | Full policy worker route, safe file opening, public-entry tests. |
| `crates/duduclaw-gateway/src/knowledge/extract_spawn.rs` | Structured fd-only knowledge extraction; no legacy process fallback. |
| `crates/duduclaw-gateway/src/knowledge/pipeline.rs` | Document mandatory custom policy worker preflight. |
| `crates/duduclaw-gateway/src/media.rs` | Isolated resizing; propagate failure rather than original-image fallback. |
| `crates/duduclaw-gateway/src/server.rs` | Block legacy soffice preview until A supplies isolated conversion. |
| `crates/duduclaw-gateway/src/tts_kokoro/mod.rs` | Refuse gateway-uid model load; TtsProvider integration seam. |
| `crates/duduclaw-gateway/src/web_extract.rs` | Bounded CSS/HTML/JSON worker adapters. |
| `crates/duduclaw-isolation/src/worker.rs` | Additive options/operations/bounded results and compatibility tests. |
| `crates/duduclaw-isolation/src/worker_media.rs` | Bounded HTML selector query types. |
| `crates/duduclaw-knowledge/Cargo.toml` | Client-only worker dependency. |
| `crates/duduclaw-knowledge/src/embed/bundle.rs` | Refuse in-process model tar unpacking. |
| `crates/duduclaw-knowledge/src/embed/local_onnx.rs` | Refuse model load and cached ONNX session use in required mode. |
| `crates/duduclaw-knowledge/src/extract/docx.rs` | Delegate truncation helper through shared extractor facade. |
| `crates/duduclaw-knowledge/src/extract/html.rs` | Delegate truncation helper through shared extractor facade. |
| `crates/duduclaw-knowledge/src/extract/mod.rs` | Direct extraction worker route and metadata conversion. |
| `crates/duduclaw-parse-worker/Cargo.toml` | Client/engine features; locked parser and syscall dependencies. |
| `crates/duduclaw-parse-worker/README.md` | Deployment inputs, startup and launcher seams. |
| `crates/duduclaw-parse-worker/src/adapter_engine.rs` | Structured parser adapters and policy/metadata tests. |
| `crates/duduclaw-parse-worker/src/adapters.rs` | Structured knowledge/table/Office policy client APIs. |
| `crates/duduclaw-parse-worker/src/archive_adapter.rs` | ZIP expansion manifest/output and shared filename decoding. |
| `crates/duduclaw-parse-worker/src/client.rs` | Admission, required latch, launcher/session seam and bounded calls. |
| `crates/duduclaw-parse-worker/src/engine_knowledge/mod.rs` | Engine-only facade over shared knowledge format-reader source. |
| `crates/duduclaw-parse-worker/src/lib.rs` | Public adapters and engine-only module boundaries. |
| `crates/duduclaw-parse-worker/src/main.rs` | Disposable fd service; sandbox, dispatch and drop-before-reply. |
| `crates/duduclaw-parse-worker/src/media_adapters.rs` | Images with output fd, HTML/CSS/search adapters and tests. |
| `crates/duduclaw-parse-worker/src/parsers.rs` | Bounded basic parser dispatch and image inspection. |
| `crates/duduclaw-parse-worker/src/parsers/archive.rs` | ZIP expansion/CRC/name/depth safety checks. |
| `crates/duduclaw-parse-worker/src/parsers/document.rs` | Bounded basic document and spreadsheet extraction. |
| `crates/duduclaw-parse-worker/src/parsers/tests.rs` | Malicious and valid basic parser fixtures. |
| `crates/duduclaw-parse-worker/src/policy.rs` | Narrower implementation limits. |
| `crates/duduclaw-parse-worker/src/response.rs` | Bound JSON serialization and closed error replies. |
| `crates/duduclaw-parse-worker/src/sandbox.rs` | Dynamic uid check, rlimits, seccomp and root resource tests. |
| `crates/duduclaw-parse-worker/src/transport.rs` | SCM_RIGHTS, seals, fd verification and output hash/readback. |
| `crates/duduclaw-parse-worker/systemd/duduclaw-parse.socket` | Fixed root socket, buffers and connection limits. |
| `crates/duduclaw-parse-worker/systemd/duduclaw-parse@.service` | DynamicUser/network/filesystem/syscall/resource sandbox. |
| `crates/duduclaw-parse-worker/tests/systemd.rs` | Six ignored real-systemd tests; one further ignored resource test is in sandbox.rs. |
| `docs/README.md` | Architecture note index. |
| `docs/architecture/native-linux-isolation.md` | Internal Package E implementation, seams, tests and handoff. |
| `docs/features/05-security-defense.md` | English unreleased behavior and acceptance boundaries. |
| `docs/features/ja-JP/05-security-defense.md` | Japanese behavior and acceptance boundaries. |
| `docs/features/zh-TW/05-security-defense.md` | Traditional Chinese behavior and acceptance boundaries. |
| `wiki/impl/native-linux-isolation-package-e-20261105.md` | Removed; relevant historical evidence and handoff moved into this Package E section. |
