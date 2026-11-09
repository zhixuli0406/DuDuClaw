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
trigger, and catalog columns absent from a changed table). The report never contains
SQL text or row content; unexpected names that are not plain identifiers are shown
as `<unprintable name>`. Mappings: v1.80 sessions also skip unpublished replies
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
The users, channel-settings, workflow and autopilot stores still
have a single `*-v1793.sql` catalog each, compared byte-for-byte as before; whether
their v1.80 stores differ has not been reviewed (for example, the v1.80.0
`mfa_email_codes`, `mfa_passkeys` and `remembered_*` tables are not in
`users-v1793.sql`). Extra tables,
columns, indexes, views or triggers reject the whole source; other historical
variants are deliberately rejected until exact catalogs are reviewed.
`tasks.db` additionally has a third catalog, `tasks-v181.sql`: the v1.80.x store plus the
P1e `task_store_markers` table that every fresh v1.81 `tasks.db` creates, so a home made
by this release is accepted as a source. Both the v1.80 and v1.81 catalogs are accepted
(exact match only, `tasks-v180.sql` is unchanged), share the v1.80 mapping, and the
marker table is never authorized for reading. `scripts/capture_tasks_schema.py`
regenerates both files; the gateway test compares `tasks-v181.sql` with a fresh `TaskStore`.
`approvals.db` likewise has a third catalog, `approvals-v181.sql`: the v1.80.x store plus
the SR5 `approvals.employee_incarnation` column (a card bound to its employee's
incarnation). v1.79.3, v1.80.x and v1.81 are accepted (exact match only;
`approvals-v1793.sql` / `approvals-v180.sql` are unchanged and no longer regenerated),
v1.81 shares the v1.80 mapping and the new column is never authorized for reading.
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
