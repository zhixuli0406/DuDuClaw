# duduclaw-import

A migration worker that reads read-only file descriptors and emits separate
bounded content records or inert authority proposals. It never writes gateway
authority.

This package implements the parser side of native Linux migration (design r7,
§10.2). A root launcher must create a network-isolated DynamicUser service,
validate and pass read-only source descriptors, and consume output only after
successful worker exit. The launcher is implemented in `duduclaw-migration::import_launcher`; final signed
installer/platform integration is still pending.

## Install

Build with the workspace Rust toolchain and bundled SQLite. The binary refuses
non-Linux execution and refuses root, missing NoNewPrivileges, or non-regular
writable source descriptors. See `systemd/duduclaw-import@.service` for the required
sandbox profile; passing inherited descriptors is a launcher responsibility.
The launcher allows 1 GiB of cgroup memory: the private tmpfs copies of main and
WAL (up to 256 MiB each) count against this limit, alongside the bounded SQLite
heap and output. It hides the actual fresh home and quarantine directories;
optional default paths need not already exist.

## Usage

The library accepts a validated `ImportRequest`, read-only readers for the main
file and optional WAL, and an output writer. The binary receives one bounded JSON
request on stdin, main input on fd 3, optional WAL on fd 4, and emits JSONL on
stdout. No paths, commands, credentials or destination locations are accepted.
The installed launcher uses `--handoff <root-owned-runtime-socket>`: the worker
authenticates root and receives SCM_RIGHTS source FDs, then a bounded request,
returning bounded JSONL on that connection. A parser/schema rejection exits 4;
isolation or I/O failure exits 3. The root relay only skips the former after
verifying the complete worker sandbox and exit state.

The inherited-fd compatibility mode closes descriptors above fd 4 before parsing, refuses an
unrequested WAL descriptor, and checks required slots exist before taking Rust
ownership. A missing close_range syscall fails closed.

Supported sources are inert UTF-8 memory/wiki/conversation/task/audit text and the
sessions and memory schemas at user_version 0. Attachment
and work-product sources emit only a root-bound handle, size and recomputed SHA-256;
the root copier publishes bytes after employee allocation is confirmed. SQLite is copied into the
worker's private temporary directory, with WAL replay only on that copy.

Supported source versions: `sessions.db`, `tasks.db`, `cron_tasks.db`, `memory.db`
and `approvals.db` are accepted from **v1.79.3** and **v1.80.x** (1.80.0/1.80.1 share
one catalog). Each version has its own pinned catalog (`session-v1793.sql` / `session-v180.sql`,
`tasks-v1793.sql` / `tasks-v180.sql`, `cron-v1793.sql` / `cron-v180.sql`,
`memory-v1793.sql` / `memory-v180.sql`, `approvals-v1793.sql` / `approvals-v180.sql`) and its
own column mapping (`src/catalog.rs` detects the version). Detection is exact: the
source's whole `sqlite_schema` must equal one catalog row for row (sessions also
with the exact reviewed `channel_settings` extension); the closest catalog is used
only to describe a refusal, never to accept. An unknown schema exits as a schema
rejection, and the worker writes a bounded report to stderr naming the closest
version and the differing objects (missing/unexpected/changed table, index, view or
trigger (unknown names appear only as ordinals and SHA-256 prefixes), and catalog columns absent from a changed table). The report never contains
SQL text, row content or unexpected source names; unknown names are represented
only by a stable ordinal and the first 16 hex characters of their SHA-256. Mappings: v1.80 sessions also skip unpublished replies
(`delivery_pending`); tasks exclude A2A-owned goals in both versions (owner
column/registry in v1.80, `created_via='a2a'` or an `a2a:` creator in both, inherited
by child tasks); cron actor columns (`on_behalf_of`, and v1.80 `created_by_user_id`
/ `scoped_by_user_id`) are never authorized for reading. The v1.79.3 sessions,
tasks and cron catalogs were reconstructed from the v1.80 catalogs and the
changelog, not captured from a v1.79.3 build; each file header says what was
removed and why. Verify them against a real v1.79.3 home before release.
The newest memory catalog adds only the RM-039 index `idx_memories_subject_ts`, and
the newest approvals catalog only the RM-027 decision-lease columns
`approvals.lease_token` / `lease_until`; both versions use the same column mapping
(memory reads the same content columns, approvals only ids and the authority schema
version), so the lease columns are never authorized for reading. These two `-v180`
catalogs describe the current fresh stores; whether a released v1.80.1 home already
had these objects has not been verified, so a v1.80.1 home without them must match
the `-v1793` catalog exactly or it is refused.
`autopilot.db` has a second catalog, `autopilot-v182.sql`: the v1.79.3 store plus the
v1.82 approval-gate ledger table `autopilot_approval_dispatch` (frozen approval-gated
actions). Both are accepted (exact match only; `autopilot-v1793.sql` is unchanged), and
the ledger is never authorized for reading: only rule ids and names are read, as before.
The gateway test compares `autopilot-v182.sql` with a fresh `AutopilotStore`.
The users, channel-settings and workflow stores still
have a single `*-v1793.sql` catalog each, compared byte-for-byte as before; whether
their v1.80 stores differ has not been reviewed (for example, the v1.80.0
`mfa_email_codes`, `mfa_passkeys` and `remembered_*` tables are not in
`users-v1793.sql`; neither are the v1.82 `mfa_bootstrap_tokens`, `mfa_bootstrap_state`
first-administrator latch and `mfa_reset_audit_outbox` tables, so a v1.82 `users.db` is refused
as a source and the latch never travels through import — the new installation keeps its own). Extra tables,
columns, indexes, views or triggers reject the whole source; other historical
variants are deliberately rejected until exact catalogs are reviewed.
`tasks.db` additionally has a third catalog, `tasks-v181.sql`: the v1.80.x store plus the
P1e `task_store_markers` table that every fresh v1.81 `tasks.db` creates, so a home made
by this release is accepted as a source. Both the v1.80 and v1.81 catalogs are accepted
(exact match only, `tasks-v180.sql` is unchanged), share the v1.80 mapping, and the
marker table is never authorized for reading. `scripts/capture_tasks_schema.py`
regenerates both files. A fourth catalog, v1.82, is `tasks-v181.sql` plus the additive
`tasks.source_origin_proof` column (`TASKS_V182` in `catalog.rs`; no separate file): the
opaque id of the gateway-held proof of an AI-created goal's source conversation, mapped as
v1.80; the reader never selects it. The gateway test compares that v1.82 catalog with a fresh `TaskStore`.
`sessions.db` v1.82 is either pinned v1.81 shape (fresh / upgraded) plus the gateway-owned
`schedule_origin_proofs` table (`session-origin-proofs-v182.sql`, the same text as
`recipient_guard::origin::SCHEMA`; `SESSION_V182` / `SESSION_V182_UPGRADED` in `catalog.rs`).
Both map as v1.81 and the proof table is never authorized for reading. The gateway tests
compare both shapes with a fresh and an upgraded `SessionManager`.
`approvals.db` likewise has a third catalog, `approvals-v181.sql`: the v1.80.x store plus
the SR5 `approvals.employee_incarnation` column (a card bound to its employee's
incarnation). v1.79.3, v1.80.x and v1.81 are accepted (exact match only;
`approvals-v1793.sql` / `approvals-v180.sql` are unchanged and no longer regenerated),
v1.81 shares the v1.80 mapping and the new column is never authorized for reading.
`mcp_keys.db` (v1.82, RM-134: dashboard MCP API keys stored as SHA-256 fingerprints) has
one exact catalog, `mcp-keys-v182.sql`, a byte copy of `duduclaw_core::mcp_key_store::SCHEMA_SQL`
(duduclaw-auth's `importer_catalog_matches_a_fresh_store` test checks the bytes and compares the
schema with a fresh store). Unlike the other authority stores it pins `PRAGMA user_version = 1`
(set by the catalog itself); any other version is a schema error. Only `name` and `deleted_at`
are authorized for reading; the internal-key, pending-legacy, ignored-entry and notice tables
are never read. `mcp_keys_usage.db` (last-use telemetry) authenticates nothing and has no
catalog: it is not imported. Likewise `mcp_auth_failures.jsonl` and its rotated
`mcp_auth_failures.jsonl.1` (v1.82 per-minute detail of refused MCP credentials, peer
addresses included) are local diagnostics, not authority: they are not imported.
A fingerprint is never evidence of authority in the new installation: every key that is not
deleted becomes a `Connector` re-issue obligation named `mcp_key:<name>` with
`requires_reentry`; only `name` and `deleted_at` are authorized for reading.
`memory-v180.sql` is reproducible from the reviewed fresh engine SQL with
`scripts/capture_memory_schema.py` (and `approvals-v181.sql` with
`scripts/capture_approval_schema.py`); the gateway regression tests compare these
fixtures against an actual fresh `SqliteMemoryEngine` / `ApprovalStore`. Memory emits only live,
non-quarantined memory text and key-fact text, never old trust or provenance. No best-effort schema guessing is performed.

Records are provisional until exit succeeds: a consumer must stage the complete
stream before publishing, and discard it if a later row fails validation.
The receiver in `duduclaw-migration::import_receiver` enforces this and checks
stream digest, counts, sequence, category and host-bound source/employee IDs.
Unknown formats are refused. Authority source kinds use their separate proposal
schema; they can never be decoded as content ImportRecords. The root launcher
must classify the source path; selecting `memory_text` does not make an arbitrary
file safe to import. Text and wiki metadata are inert content, never configuration.

Authority extraction accepts known config/agent/org TOML, MCP/settings and
category-specific JSON, instruction/skill text and exact users/tasks/approvals/
cron SQLite catalogs. Pure-time CronStore rows and legacy cron JSONL become
separate schedule proposals without old execution state, actor or delivery grants.
Workflow/command triggers and unknown JSONL fields require re-entry. Actual
remote MCP, personal/OAuth credentials and channel stores are inventoried;
encrypted website credentials and old integrity baselines produce re-entry
notices without being decrypted or trusted. Source values never become live
configuration during import. Credential
fields are masked and marked for re-entry; original passwords, TOTP, sessions and
tokens are excluded. Unknown configuration sections yield only a name and a
re-entry requirement. Separate DB read-only grants and positive daily/monthly
budgets are removed from the Employee proposal so they need their own decisions.
The explicit safe config reader covers container sandbox, computer-use image/
workspaces and provenance. Empty MCP `env: {}` remains actionable; non-empty env
and unknown server fields require re-entry. Unsupported historical DB variants are rejected. Not all
category-specific gateway apply backends are implemented; see the Package G
handoff for the exact remaining categories.

The exact workflow and autopilot catalogs now produce a separate re-entry item
for each latest approved/candidate workflow revision and each automation rule.
Pending reminders are inventoried individually from bounded JSONL. Only labels,
kind and workflow revision cross this path; old grants, acceptance, action
payloads, recipients and execution history stay quarantined. These items still
require fresh setup and cannot restart old work. Catalogs are reproduced with
`scripts/capture_schedule_schemas.py` and checked against normal gateway stores
by the remote-only gateway test. Employee action rules and trusted read-hint
servers are separate Policy proposals requiring re-entry, not part of employee
allocation. URL credentials in userinfo, query or fragment are masked.

The output contract bounds records to 64 KiB, streams to 64 MiB and 100,000
records. Source files are at most 256 MiB each. Every record is bound to the host
migration/employee IDs; the gateway assigns legacy_import provenance and minimum
trust independently. Raw SQLite errors and source text are never diagnostics.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-import --offline
```

## License

Apache-2.0.

## R5 front half: read-only v1.82 schema capture

The migration target is v1.83.0. A supported single-binary host must first
upgrade in place to v1.82.x. The creating-version matrix is v1.80.0, v1.80.1,
v1.81.0 and v1.82.0, each **after** upgrading to v1.82.x; v1.79.3 is excluded.
The old parser catalogs remain available for existing callers, but are not
proof of eligibility for this new upgrade path. No installer, upgrade routing,
root migration pipeline, materialization or confirmation behavior is added here.

`src/expected.rs` is the hand-off API. `SETS` contains byte-pinned,
code-derived *candidates*. It reuses the existing SQL and retains V180/V181
column mappings when no readable content column changed. `release_captures()`
contains 40 explicitly unfilled creating-version/store slots; `expected_sql`,
`user_version` and `application_id` are `None`, and `matches()` always refuses
an unfilled slot. The later pipeline must consume verified release slots,
not accept a code candidate as release evidence. Users and causal-memory also
have named `NEEDS-RELEASE-CAPTURE` candidates with no SQL; an empty database
cannot match a placeholder. No release-capture slot has been verified yet.
When evidence is reviewed, pin SQL as `<store>-<creating-version>-to-<last-version>.sql`
(with an actual last patch version), store the captured pragma values and
provenance, and test exact acceptance plus extra-index refusal. Never paste
untrusted SQL from an operator report into a catalog.

| Store / relative path | Code candidate and provenance | Real capture still required |
| --- | --- | --- |
| `sessions.db` | `SESSION_V182` and `SESSION_V182_UPGRADED`: gateway `SessionManager` CREATE/ALTER order plus `recipient_guard/origin.rs` SCHEMA | Each creating version, including channel-settings extension; actual fresh/upgraded ordering |
| channel settings (inside `sessions.db`) | `ChannelSettingsManager::from_session_db`, existing `channel-settings-v1793.sql`, combined with both session shapes | Same session matrix; no separate physical database is assumed |
| `tasks.db` | `TASKS_V182`: gateway `task_store/schema.rs`, marker table and `source_origin_proof` ALTER | Each creating version and shipped labels |
| `cron_tasks.db` | `CRON_V182`: gateway `cron_store.rs` initializer, ordered TEXT-column additions, receipts and scoped indexes | Each creating version; historical v182 label was unverified |
| `approvals.db` | `APPROVALS_V181`: gateway `approval/store.rs`, lease and employee-incarnation migrations | Each creating version; `-v180` lease label is not proof of v1.80 shipping shape |
| `memory.db` | `MEMORY_V180`: `SqliteMemoryEngine` in memory `engine.rs`, without causal tables, user_version 0 | Each creating version; causal same-file shape/user_version 1 is a separate placeholder; do not broaden the content reader until verified |
| `users.db` | Historical `users-v1793.sql` is a closest-catalog diagnostic only; **no v182 candidate** | Auth `db.rs`, MFA/bootstrap/reset-outbox, remembered-session and isolation migrations have history-dependent ordering. Capture real released homes; refusal means **no user account proposals** |
| `workflow.db` | Existing `workflow-v1793.sql`: gateway workflow `store.rs` and `draft_store.rs` initializers; design records unchanged DDL | Each creating version; no new mapping |
| `autopilot.db` | `autopilot-v182.sql`: gateway `AutopilotStore`, including `DISPATCH_SCHEMA_SQL` | Each creating version; approval dispatch remains unreadable |
| `mcp_keys.db` | `mcp-keys-v182.sql`: core `mcp_key_store::SCHEMA_SQL`, user_version 1 | Each creating version; no key material is captured |
| `googlechat_inbox.db` | Wave-1 `googlechat-inbox-v182.sql`: gateway inbox initializer | Each creating version, when enabled; inventory only, never import/replay delivery state. v183 placeholder catalogs are not v182 candidates |

### Report and refusal contract

Both capture wrappers fix `PATH` to `/usr/bin:/bin` and require `CAPTURE_BIN`
to be an absolute path to a regular executable file that is not a symlink.

The separate operator binary is `duduclaw-capture-legacy-schemas`; the existing
sandboxed `duduclaw-import` fd worker keeps its default invocation. The new
binary takes `<home> --created-with <v1.80.0|v1.80.1|v1.81.0|v1.82.0>`.
It is supported on native Linux; Unix library tests also exercise file safety.
It creates no files in the supplied home. The home itself and store/sidecar
opens use no-follow directory descriptors; source files must open read-only,
be regular, have exactly one hard link and be at most 256 MiB.
Run as root or as the home's owner: Linux requires `O_NOATIME` for source and
sidecar opens; an `EPERM` refusal produces `refused_noatime`, with no retry that
could update access times. Linux copies bytes only into an anonymous memfd,
then applies `F_SEAL_SHRINK|F_SEAL_GROW|F_SEAL_WRITE|F_SEAL_SEAL`. SQLite opens
`/proc/self/fd/<n>` read-only with `mode=ro&immutable=1`; a capture-only VFS
preserves this generated path instead of resolving the procfs magic link, and
provides only sealed read-only descriptor I/O (no file creation, WAL or mmap).
No named copy is created on Linux, including on failures or process termination.
The non-Linux Unix development/test fallback ignores `TMPDIR`: it creates a
0700 directory under the fixed, verified root-owned `/tmp` (canonicalized on
macOS), verifies that the private directory and the captured home do not overlap
in either direction, and creates `source.db` with 0600 and `O_EXCL|O_NOFOLLOW`.
RAII removes that directory on success and error returns (unlike memfd, a forced
process termination can leave this development-only named fallback behind).
The normal importer heap, schema-row, SQL-length,
column and query-time limits apply. Workflow keeps the existing reviewed
expression-depth exception. The capture queries only schema and pragma metadata.
It does not run quick_check or read application/FTS data rows.

Stop **all** writers before capture, including gateway, timers and CLI/MCP
processes. Nonempty WAL or rollback journals refuse: immutable SQLite must not
silently omit pending schema changes. SHM, even though unused, must be a bounded
regular non-symlink file. A WAL-bearing customer home is not repaired by this
tool: arrange a normal clean shutdown under the operator's existing procedure,
or investigate it separately. Never delete sidecars to force a capture.
Before and after copying, capture compares the store's device/inode, size,
mtime and ctime (seconds and nanoseconds), link count, all three sidecars'
existence and identities, and the parent directory's device/inode (including
reopening the home to detect replacement). Sidecars with multiple links refuse.
This is not a live snapshot protocol; writers must stay stopped throughout capture.

One deterministic UTF-8 JSON object is written to stdout, at most 128 KiB,
with stable path/placeholder ordering and no timestamps or absolute host paths:

- Top-level: `format_version` (1), `source_version` (matrix endpoint v1.82.x,
  an operator assertion, not an inferred binary version), `migration_target`,
  `creating_version`, `release_matrix_verified` (false), `stores`, `pending_captures`,
  `private_copy` (`sealed_memfd` on Linux, `private_0700_directory_0600_file` on
  other Unix development/test platforms).
- Per store: `path`, `status`, `user_version`, `application_id`, `schema_sha256`,
  `catalog`, `exact_catalog`, `expected_candidate`, `differences`, `omitted_differences`.
- Fingerprint: lowercase SHA-256 of compact serde_json serialization of the
  complete ordered `(type,name,tbl_name,sql)` schema tuples (including null SQL
  for SQLite autoindexes), using the existing schema query's stable ordering.
  SQL and rows themselves are never emitted. At most 16 differences appear.
  An unknown source object is represented only as `unexpected <kind>
  ordinal=<n> name_sha256=<hex>`: kind is a fixed table/index/trigger/view label,
  ordinal is one-based among unknown objects sorted by kind/name, and hex is
  the first 16 lowercase hex characters of SHA-256 of the exact UTF-8 name.
  Source names are never emitted. Missing and changed entries use trusted
  catalog names; changed entries require an exact kind/name catalog match.
- Catalog id: trusted relative filename, label, zero-based catalog ordinal and
  optional `:channel_settings`; ordinal distinguishes same-label fresh/upgraded
  shapes. `exact_catalog=false` means closest-only diagnostics, never acceptance.
- `absent` records an optional missing file; `code_candidate` matches a code
  candidate exactly including pragmas. `catalog_only_not_v182_expected` records
  an old exact catalog without a v182 candidate; `unrecognised` records drift.
  `refused_*` records unsafe opens, sidecars, limits, malformed SQLite or changes
  during copy. `refused_hardlink` records any store/sidecar link count other
  than one; `refused_noatime` records Linux `O_NOATIME` permission refusal.
  Failure values never echo filesystem/SQLite error details.

Exit 0 means inventory completed with only absent stores/code candidates;
it **does not** approve migration or prove every feature store was initialized.
Exit 4 still writes the report but contains refused/unrecognised stores. Exit 3
means capture could not complete; discard stdout. All unknown schemas remain
refused, and all unfilled release-matrix slots remain refused even on exit 0.

### Reviewer smoke run (not executed by the implementer)

In a disposable Linux VM, build the capture binary with
`cargo build -p duduclaw-import --bin duduclaw-capture-legacy-schemas`.
Use an official, signature-verified v1.82.0 gateway binary for this fresh fixture:
`DUDUCLAW_V182_BIN=/absolute/v182/duduclaw CAPTURE_BIN=/absolute/target/debug/duduclaw-capture-legacy-schemas bash scripts/live-test/linux-protected/capture-v182-home.sh > /tmp/r5-report.json`.
The script calls `make-home.sh`, uses isolated HOME/DUDUCLAW_HOME with the
production-shaped wildcard-allowlist employee, waits for loopback health,
stops the gateway, then captures. It retains the disposable fixture/logs in
`/tmp` for reviewer inspection; only stdout should be returned. Lazy stores
may be absent: this smoke fixture is not the complete signed-release matrix.
Unit tests live in `capture_tests.rs` and `expected.rs`; reviewer command:
`cargo test -p duduclaw-import`. None of these Rust commands or the live script
were executed during implementation.

### Real release-matrix/operator capture

Use disposable Linux VMs for the release matrix. Download official v1.80.0,
v1.80.1, v1.81.0 and v1.82.0 tarballs and their minisign signatures; verify
against the independently trusted release public key before extracting or
executing anything. Do not use this development checkout as a historical binary.
For each creating version X, use `make-home.sh` with a new empty home and
isolated OS HOME. Boot X to health, initialize the applicable feature stores,
stop cleanly, then boot/stop each later official release in order through the
chosen v1.82.x endpoint. Capture only the endpoint (Q3), including stores
created lazily by Google Chat, workflow, autopilot and causal features where
applicable. Keep release versions, verified artifact hashes, feature-initializer
steps and CREATE/ALTER provenance in the PR evidence; don't send customer logs.
This package intentionally does not automate downloads, minisign trust setup,
feature seeding, host upgrades or migration.

For an existing real host already upgraded to v1.82.x, with all writers stopped,
run the wrapper as an account with read access:
`CAPTURE_BIN=/absolute/duduclaw-capture-legacy-schemas bash scripts/live-test/linux-protected/capture-legacy-schemas.sh /absolute/legacy-home v1.80.0 > /outside/legacy-home/schema-report.json`.
Replace the final argument with the documented original creating version; if
that history is unknown, establish it before assigning a matrix slot. Write the
report outside every customer home. Send back **only the JSON report**, including
exit-4 reports; no databases, SQL dumps, config, identities, keys, credentials,
logs or home archives. Reports contain fingerprints rather than raw SQL so a
planted schema cannot disclose a secret via DDL. Missing catalog SQL must be
reproduced from the verified release VM and reviewed locally, using the report
to compare fingerprints; operator reports are diagnostics, never trusted DDL.
