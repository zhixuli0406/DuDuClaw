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
fresh-install sessions and memory schemas at user_version 0. The `*-v1793.sql`
file names are historical: sessions, tasks and cron catalogs track the current
(v1.80) fresh stores, so older variants of those stores are refused. Attachment
and work-product sources emit only a root-bound handle, size and recomputed SHA-256;
the root copier publishes bytes after employee allocation is confirmed. SQLite is copied into the
worker's private temporary directory, with WAL replay only on that copy. Schema
rows are compared byte-for-byte with the trusted `session-v1793.sql` and
`memory-v1793.sql` schemas (plus the exact reviewed `channel_settings` extension
for the shared sessions database);
extra tables, columns, indexes, views or triggers reject the whole source. Older
schema variants are deliberately rejected until exact catalogs are reviewed.
`memory-v1793.sql` is reproducible from the reviewed fresh engine SQL with
`scripts/capture_memory_schema.py`; the gateway regression test compares that
fixture against an actual fresh `SqliteMemoryEngine`. Memory emits only live,
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
