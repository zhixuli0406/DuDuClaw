# Package H: protected-mode production selectors for the 29 non-connector tools

Status: plan, 2026-11-08 (owner decision D-4: in scope for v1.81.0). Base `feat/iso-v181-20261108`.

## Why they are refused
- `human_authorization` (`isolation_mcp/boot.rs:318-357`) never emits `Workspace` or `UrlOrigin` grants, so
  `tool_policy::eligible` (`duduclaw-mcp-boundary/src/tool_policy.rs:385-393`) is false; the CLI hides and refuses them
  (`mcp_isolation.rs:226-245`, `mcp_dispatch.rs:644-647`).
- No production `ResourceRoots::insert` for `employee-work` (`resources.rs:407`); no root receipt producer
  (`duduclaw-execution/src/spawnd.rs:134`, only a test implementor).
- Structural: the employee work dir is employee-uid 0700 (`spawnd/src/storage/adapter.rs:269-271`), mounted only during a
  run's storage lease (`:152-176`); the gateway cannot open it. A selector must go through root, for the live run only.

## Tools (route; mechanism)
1–3 file_read / csv_read / xlsx_read: root work broker fd → (parse-worker ReadTable for csv/xlsx).
4–5 web_fetch_cached / web_extract: `PublicOrigins` grant + host `web_fetch_protected_cached` (DNS pin, no redirects) → parse-worker ExtractHtml.
6–8 execute_program / code_map / office_script: spawnd ExecutionBackend child run (offline), pending job after parent exits.
9–10 browser_record_start/stop: child run via duduclaw-egress; job store stop.
11 skill_from_recording: job store + brokered /work artifacts (fix snapshot path mismatch `isolation_worker_plans.rs:616-642` vs `recording_helper.rs:110`). SFR (2026-11-09): HX6's `SqliteRecordingJobs` installed at boot as the H12d seam (own finished job only: employee + principal + binding, exact `(run, call)`); artifacts only as `VerifiedArtifact` and still untrusted, parsed only by E (trace archive caps, no traversal/symlinks); E-scanned publication into own SKILLS; signed only behind the HX6 recording gate (hidden while it is off); off the pending list.
12–15 skill_extract / shared_skill_adopt / skill_security_scan / skill_graduate: parse-worker E ops + skill publication into own wiki.
16–18 skill_synthesis_run / skill_hub_install / skill_curator_status: host provider/fetch + E ops; curator own skills only.
19–21 create_task / spawn_agent / spawn_ephemeral: host queue; live task-ownership; ephemeral under parent identity.
22–26 discovery_catalog/list/tree/artifact/cancel: host discovery service, own jobs.
27–29 inspect_branches / diff_branches / fork_cost: host fork-store reads.

## Decisions (H0)
- D-H1 workspace authority: root work broker (opens a file under the live run's attached mount, passes the fd) + per-employee
  root-owned work anchor dir as `employee-work` identity. Rejected: POSIX ACLs for the gateway; CAP_DAC_READ_SEARCH.
- D-H2 URL authority: new MAC-covered grant `PublicOrigins` (tag 11, only with EmployeeScope); egress/fetch keep
  public-address-only.
- D-H3 spawn_ephemeral under the parent employee's uid/storage/binding; tools = requested ∩ parent; Personal parent owner-only.
- D-H4 curator `run=true` limited to the caller's own skills.
- D-H5 execution-type tools return accepted/pending jobs that start after the parent run exits (D8: one capability-holding run per employee).

## Tasks
H1 IPC ops OpenWorkFile/WorkAnchor (`duduclaw-isolation/src/ipc.rs`). H2 broker core `spawnd/src/work_broker.rs` (openat2
BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV; S_ISREG, nlink==1, owner uid, device, ≤16 MiB). H3 work anchor
`spawnd/src/work_anchor.rs` + adapter hooks. H4 spawnd wiring (host.rs, host_worker.rs, server.rs, admission.rs, audit.rs;
root test `spawnd/tests/work_broker_chain.rs`). H5 execution client + `SpawndWorkspaceBridge` (`duduclaw-execution`).
H6 brokered root in `mcp-boundary/src/resources.rs` (`WorkBroker` trait, `insert_brokered`). H7 PublicOrigins grant
(capability.rs tag 11, tool_policy.rs, resources.rs; CHANGELOG). H8 create_task live task selector (tool_policy.rs:433-437,
isolation_resources.rs:79-131). H9 issuer selectors `isolation_mcp/selectors.rs`. H10 issuer hook (boot.rs, after F1).
H11 file tools (`mcp_files.rs`). H12a curator, H12b hub install, H12c synthesis, H12d skill_from_recording. H13
spawn_ephemeral. H14 parity + docs (empty `pending_production_selectors`, tools matrix). H15 live acceptance
(`scripts/live-test/linux-protected/package-h-acceptance.sh`, `tests/iso_package_h_chain.rs`). H16 readiness reasons
(`workspace_broker_unavailable`, `helper_runtime_missing{runtime}`).

Waves: W3 H1,H6 · W4 H2,H7 · W5 H3,H8 (+SR-H1) · W6 H4,H5,H11 · W7 H9,H13,H12a · W8 H10,H12b,H12c,H12d · tail H16→H15→H14.

## Infrastructure
Signed helper runtimes (duduclaw-program-v1, code-map-v1, office-<skill>-<script>-v1, browser-record-v1 with Chromium +
egress profile; `scripts/sign-isolation-manifest.py --kind runtime`), parse-worker runtime ops, public HTTPS fixture origin
(redirect, DNS rebinding, skill-hub archives incl. zip bomb/traversal), LLM provider key, GCE VM for metadata address.
Acceptance matrix additions 16–27 (see planning report in session log).

## Estimate
+4 / +7 / +10 working days ⇒ total 14 / 21 / 30 (optimistic / expected / pessimistic).

## Owner decisions (2026-11-08)
- D-H1 root work broker: approved. D-H2 PublicOrigins grant: approved. D-H3 ephemeral under parent identity: approved.
  D-H4 curator own skills only: approved.
- **D-H5 rejected: execution-type tools (execute_program, code_map, office_script, browser_record_*) must return results
  synchronously in protected mode.** This reopens D8 (one capability-holding run per employee). A design revision is required
  (preferred direction: run the tool as a bounded child inside the SAME live run's sandbox/storage lease rather than a second
  capability-holding run), followed by a Daybreak threat-model review and owner confirmation before H4/H5 coding.
- **D8 revision (owner 2026-11-08): option A** — execution-type tools run as bounded helper processes inside the calling
  run via an in-run `duduclaw-helper-launcher` forked by the trampoline init (template: egress forwarder branch,
  `duduclaw-run/src/linux/runner.rs:388-402`); one capability-holding run per employee is preserved; MCP call blocks until
  the helper exits; timeout ≤ remaining run time; `OOMPolicy=continue` on the unit; helper allowlist only narrows the parent
  profile; egress helpers need an Egress parent. **Recording lifetime: at most the calling run**; flushed on run end, a later
  stop returns the stored result. New tasks HX1 (design + contract amendment, Daybreak review) … HX6 (recording flush);
  pending-job paths for execute_program/code_map/office_script dropped. Estimate total 16 / 25 / 36 working days.
