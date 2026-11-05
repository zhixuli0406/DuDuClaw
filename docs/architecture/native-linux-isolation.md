# Native Linux isolation implementation

## Package G — migration from v1.79.3

### Round 3b status (2026-11-05)

Phase 1 completed: inspected the inherited tracked diff and untracked Package G
files on `feat/iso-g-20261105`; no reset or deletion of the interrupted work was
needed. The requested offline test command passed: importer 35 (31 unit + 4
integration), migration 34, isolation 29; **98 passed, 0 failed**. `git diff
--check` also passed. No gateway/CLI build or commit was performed.

Phase 2 completed within its 25-minute budget (about 01:09–01:24 UTC). The
inherited launcher/startup code was retained and hardened; this round did not
replace the missing F/B/H integration. **110 portable tests passed, 0 failed**:
importer 41 (37 unit + 4 integration), migration 40 (38 unit + 2 integration),
isolation 29. The exact command was:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-import -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline
```

Both passed; the second is Linux **type checking**, not native execution. Existing
unsafe-code/dead-code warnings remain. Logs:
`/private/tmp/iso-g-round3b-final.log` and
`/private/tmp/iso-g-round3b-linux-final.log`. No gateway/CLI build, `cargo fmt`,
commit, native service start or production migration was performed.

Round 3b changes and evidence:

| Files (relative to repository root) | Change |
| --- | --- |
| `crates/duduclaw-migration/src/import_manifest.rs`; gateway `src/handlers/migration_authority.rs` | Extracted the actual inert proposal-staging function into the small migration crate; gateway delegates to it. Rejects mixed migrations and duplicate source records before staging. |
| `crates/duduclaw-migration/tests/authority_extraction.rs`; migration `Cargo.toml`; `Cargo.lock` | Real extractor → serialized proposals → actual staging store → reopen/replay integration test for all 17 categories, including planted Admin/password and stdio MCP. No live account/config/employee files are created; unconfirmed apply never reaches the applier. Importer is a test-only dependency. |
| `crates/duduclaw-import/src/authority_schedules.rs`; `src/lib.rs`; `src/authority.rs`; migration `src/root_pipeline.rs` | Exact-schema workflow/autopilot readers and bounded reminders JSONL reader; proposals are per-record inert metadata with mandatory re-entry, never old actions, acceptance, grants, leases or delivery routes. Root inventory and WAL transport route to these readers. |
| `crates/duduclaw-import/scripts/capture_schedule_schemas.py`; `src/autopilot-v1793.sql`; `src/workflow-v1793.sql`; gateway `src/migration_import.rs` | Reproducible schema catalogs and remote gateway catalog-drift assertions against the normal store constructors. Catalog generation succeeded; gateway assertions are written but not run locally. |
| `crates/duduclaw-import/src/authority_config.rs`; `src/authority.rs` | Employee action rules/trusted-read hints become separate inactive Policy proposals requiring re-entry; malformed shapes are withheld. MCP URL fragments join userinfo/query redaction. Focused canary tests pass. |
| `crates/duduclaw-migration/src/import_profile.rs`; `src/import_launcher.rs`; `src/lib.rs`; importer `systemd/duduclaw-import@.service` | Portable sandbox readback checks; add arm64 `mkdirat`; test reference-unit parity and reject widened/missing properties. Accept only the explicit syscall list plus the reviewed fixed systemd v249/v252/v255 implicit defaults, never arbitrary groups. Actual DynamicUser launch is still unverified. |
| `crates/duduclaw-migration/src/import_manifest.rs`; gateway `src/migration_startup.rs` | Shared pure boot decision with portable tests: explicit import requests cannot fall back to normal startup, and an unfinished manifest for this home blocks ordinary boot. Gateway/CLI startup process tests still require the remote builder. |
| `CHANGELOG.md`; importer `README.md`; security-defense feature docs in all three languages; this document | Updated unreleased behavior and evidence boundaries. |

The scheduling readers deliberately do not restore executable workflows or
reminder delivery. Full category **extraction coverage** is tested; full category
**apply/readback** is not complete. Remaining priority 4/5 work is unchanged:
F/B/H installed platform adapter and employee/principal allocation, confirmed
content materialization, Personal fresh login, Connector and non-Telegram
activation, non-time schedules, org/action-rules/cached-config apply/readback,
first-admin issuer, and final completion/report integration. Without the platform
adapter, `run` still refuses before taking the old service offline. The exact
remote test commands below remain required.

Independent static review found and resolved an arm64 syscall omission, URL
fragment credential leakage, and systemd implicit-default readback compatibility.
The default expansion was checked against official [v249 transient-unit
code](https://github.com/systemd/systemd/blob/v249/src/core/dbus-execute.c) and
[v255 syscall definitions](https://github.com/systemd/systemd/blob/v255/src/shared/seccomp-util.c).
The final reviewer reported no remaining finding in that correction. This is not
formal Daybreak review or native systemd acceptance. During development the new
users fixture omitted a required `bound_at` column; the fixture was fixed before
the final passing run. Shared isolation wire formats/hard limits are unchanged.

The older round-3 notes below describe inherited code and historical results,
not additional round-3b acceptance evidence. In particular, their unexecuted
gateway Admin/MCP test is distinct from the new portable integration test above.

Status: **PARTIAL — round 3 adds runnable G components, but the requested full
migration and all authority apply categories are NOT complete.** No commit,
release, deployment or production migration was performed. This worktree still
lacks the F/B/H platform adapter and several normal product apply/readback paths;
`run` therefore still refuses before taking the old service offline on an install
without that adapter. This is not end-to-end migration acceptance.

### Round 3 implementation and remaining gates (2026-11-05)

Implemented in this worktree:

- A real `systemd-run` DynamicUser importer launcher. Root verifies systemd
  properties, MainPID, actual UID, capabilities, NoNewPrivileges, seccomp and a
  distinct network namespace before passing one or two read-only regular-file
  descriptors through SCM_RIGHTS. The 1 GiB cgroup memory limit accounts for the
  main/WAL copies in tmpfs. Actual home/quarantine paths are hidden and unsupported
  ProtectHome layouts are rejected before outage. Output is bounded and withheld until process
  exit/readback. Verified parser rejection is separate from fatal sandbox/IPC
  failure. Native systemd execution has not been run here.
- Exact path inventory under a pinned quarantined dirfd, bounded traversal,
  no-follow source opens, immutable source IDs, root-owned staged streams and
  source-rejection reports. CLI cache remains excluded except the exact Claude
  hook settings file. Unrecognized sources remain in recovery; attachments and
  work-product publication still require the confirmed B/H allocation path.
- An exclusive gateway startup branch before normal config/bootstrap/listeners.
  The CLI branch precedes file logging, OTLP and redaction initialization.
  It forces and verifies non-dumpable/NNP, consumes systemd FD 3 and checks a
  root-owned import manifest. Ordinary boot refuses an unfinished import plan.
  Root-peer authorization plus a per-message SCM_CREDENTIALS handshake verifies
  the actual gateway acceptor despite systemd owning the listening socket.
- Unconfirmed employees' content is archived with source IDs and durable stream
  receipts, without creating employee directories, UIDs, disks or active
  memory/wiki entries. A root-only status reply reports exact stream, record,
  pending and expired-approval counts. It is **archive acceptance**, not completed
  materialization into confirmed employees' live content namespaces.
- Worker extraction for known config/org/agent TOML, MCP JSON, instruction files,
  skill text, hook/settings JSON, roles/departments/bindings/connectors/data-source
  JSON, scheduling JSON, wiki scope policy, exact v1.79.3 users/tasks/approvals/cron
  schemas and the sessions-with-channel-settings variant. Actual remote MCP,
  personal/OAuth stores, channel bindings, empty departments and old `.soul_*`
  baselines are accounted for. Encrypted stores and unsupported authority
  artifacts produce explicit re-entry obligations instead of becoming authority.
  Users exclude password/MFA/session/token material; tasks emit inactive schedule
  proposals. Unknown schema/version, extra views/triggers and quick_check failure
  reject the entire source. Unknown config sections and secret-bearing values
  require re-entry; the worker does not put original credentials in proposals.
- Actual stdio MCP apply/readback/disable, with normal scanning and locked atomic
  config writes, baseline compare and an operation/digest receipt in the same
  artifact. Remote MCP, old environment credentials and remote bridges refuse.
  Profile instruction changes call the ordinary review/save/history/baseline path
  and reload the live registry. Neither path launches the proposed command.
- Atomic Shared ACL binding receipts in the same transaction as the ordinary auth
  binding writer. Fresh active login is required. Gateway application additionally
  requires actual Shared-principal allocation readback from B/D/H; an Applied
  Employee proposal or `shared:` label alone is insufficient. This gate currently
  refuses because the registry adapter is absent.
- Root pipeline composition through content import/confirmation readback. A
  waiting administrator is a successful `AwaitingAdministrator` result; completion
  still requires fresh zero-outstanding readback. The CLI uses the composed
  integration, not the early-stop adapter.
- Skill and hook adapters use the normal scanner/loader and guard merge paths.
  Prepared inode receipts survive publication without hard links in live paths.
  Profile history binds the operation/digest. Department publication uses
  `RENAME_NOREPLACE`, inode and operation xattr ownership. Equal normal writes
  are not adopted or removed by compensation.
- Config/Budget/policy ownership is recorded from the normal writer's fsynced
  temporary file before rename. The ordinary product-policy/MFA/Google source
  guard remains in the write path; migration refuses Google/resolver source
  changes. Compensation checks ownership again inside the normal file lock.
- Telegram channel bindings, pure time cron and read-only database grants have
  normal writers and actual router/scheduler/registry readbacks. They remain
  unavailable without the real Shared-principal backend. Ordinary binding writes
  now read/mutate/write under the same lock; unrelated operations retain receipts.
- Retrying a failed apply first authenticates the session and binding, reconciles
  partial effects, then performs fresh validation. A failed fresh validation
  leaves the item Failed and its prior partial authority disabled.

#### Authority coverage

These are code paths, not native acceptance results. Every extracted item stays
inert until confirmation; a written adapter that still requires B/D/H proof is
not an available production migration path.

| Category | Source and apply boundary |
| --- | --- |
| User | Exact `users.db`; fresh invitation-only account, new password/MFA required |
| Role | Built-in Admin/Manager/Employee role travels with User; product has no mutable custom-role registry |
| Department | Real department directories, org/user metadata; normal creation with owned-directory compensation |
| Employee | `agent.toml` pending only; confirmed immutable UID/quota allocation awaits B/H |
| EmployeeInstructions | Normal review/save/history for SOUL/IDENTITY/INSTRUCTIONS/CONTRACT; old baselines are not trusted |
| UserAgentBinding | Shared ACL adapter requires fresh login and real Shared proof; Personal reclaim remains missing |
| ChannelBinding | Actual channel store; Telegram adapter requires Shared proof; other channels require re-entry |
| LegacyApproval | Exact approvals DB and JSON; always ExpiredRejected, never reactivated |
| McpServer | Normal stdio writer/scanner, empty env supported; credential/remote records require reconnection |
| Skill | Flat Markdown or canonical SKILL.md via normal scanner/loader; extra bundle artifacts/shared skills require re-entry |
| Hook | Strict command-hook structure, normal mandatory guards, owned atomic publication; never executes during apply |
| Policy | Delegation plus explicit per-use container/computer_use/provenance readers; org/action_rules/other policies remain unsupported for apply |
| Connector | Actual personal/OAuth/website stores are inventoried; provider-specific fresh connection/apply is missing |
| DataSourceAuthorization | Separate read-only grants from old agent capabilities; normal validator/writer/registry, requires Shared proof; source definitions/secrets require re-entry |
| Budget | Positive daily/monthly caps become separate proposals; unlimited/unknown settings require re-entry |
| ConfigurationSection | Explicit safe container/computer_use/provenance sections; other cached sections need their own reload/readback adapters |
| Schedule | Actual cron DB/JSONL pure time records, normal store and live cache; Shared proof required; workflows/goals/responsibilities/command triggers remain re-entry obligations |

**Still missing (do not report Package G complete):**

1. `InstalledPlatform` is a fixed, bounded, root-owned adapter protocol, but its
   expected `duduclaw-migration-platform` executable is **not implemented or
   installed in this worktree**. F/B/H must supply signed preflight, live final
   ownership/quota/protocol checks and fresh service activation. Thus the normal
   CLI `run` remains unavailable without that integration; substituting this seam
   did not by itself satisfy the requested end-to-end flow.
2. Employee allocation and Shared/Personal principal integration; provider-specific
   Connector activation; non-Telegram channel admission; non-time scheduling
   admission; org/action_rules and other cached configuration/policy reloads.
   Shared ACL/channel/cron/database-grant adapters await immutable principal
   readback. Personal fresh-login and archive materialization remain open.
3. Historical authority schema variants beyond the explicit catalogs, external
   OAuth revoke/reconnect and per-secret rotation inventory. Exact approvals DB
   extraction is now implemented and expires every old approval. Original
   credential retention is not wired from the extractor: secret-bearing proposals
   require re-entry; a notice is not a restored provider connection.
4. First-admin bootstrap issuer/claim-route integration, final completion report,
   installed expiry timers, cross-filesystem quarantine and file-tree recovery.
5. Remote gateway/CLI compile/tests, actual v1.79.3 root migration on the three
   Linux distributions and formal Daybreak review.

Shared `duduclaw-isolation` wire formats and hard limits are unchanged. Package G
has a private manifest/handshake protocol and additive private proposal fields.
The importer adds the already-present workspace `toml` dependency and gateway
adds importer as a test-only dependency; no heavy new package was introduced.

Round 3 local results and exact remote commands are recorded below under
**Round 3 validation**. The following round-2 implementation notes are historical
background; this section supersedes their launcher/startup and support status.

### Round 3 validation

Executed locally on macOS, with two build jobs and incremental compilation off:

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth --offline` | 136 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-isolation --offline` | Earlier 33 migration + 29 isolation passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration --offline` | Final 34 passed, 0 failed, including the new partial-compensation retry case |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-import --offline` | Final 31 unit + 4 integration passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline` | Passed; unsafe-code lint warnings remain |
| `git diff --check` | Passed |

The 234 distinct final passing tests above do **not** include gateway tests. In
particular, the planted Admin/MCP test and the import-only startup tests have been
written but have not been executed here. The Linux cross-check is type checking,
not native execution. No gateway/CLI/workspace build, `cargo fmt`, remote build,
root systemd test, service deployment or production migration was run. The web
dependencies are absent (`web/node_modules`), so TypeScript/Vitest were not run.

Local logs: `/tmp/iso-g-round3-auth-full.log`,
`/tmp/iso-g-round3-migration.log`, `/tmp/iso-g-round3-migration-final.log`,
`/tmp/iso-g-round3-linux-final.log`; final importer output is in the delegated
test transcript (the earlier `/tmp/iso-g-round3-import-final.log` predates the
additional sources). During development an importer fixture INSERT and the new
retry test's error-comparison assertion failed; both were fixed and their suites
then passed. Independent static reviews closed the import-boot, secret/schema,
Shared proof, normal-write ownership and retry findings. This is not a formal
Daybreak review and does not replace gateway compilation.

Run on the remote Linux builder (not on the shared 16 GB Mac):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo build -p duduclaw-import -p duduclaw-migration
cargo test -p duduclaw-auth migration_
cargo test -p duduclaw-migration
cargo test -p duduclaw-import
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-cli --test migration_import_startup
cargo test -p duduclaw-gateway migration_confirmation_
cargo test -p duduclaw-gateway migration_decisions_
cargo test -p duduclaw-gateway migration_authority_ -- --test-threads=1
cargo test -p duduclaw-gateway migration_import_
cargo test -p duduclaw-gateway --lib agent_binding
cargo test -p duduclaw-gateway --lib departments
cargo test -p duduclaw-gateway --lib db_source_grants
cargo test -p duduclaw-gateway --lib cron_store
cargo test -p duduclaw-gateway --lib cron_scheduler
cargo test -p duduclaw-gateway --lib config_commit
cargo test -p duduclaw-gateway --lib home_publication
```

The authority filter includes these required regression cases:

```sh
cargo test -p duduclaw-gateway migration_authority_planted_admin_and_mcp_inactive_until_individual_confirmation
cargo test -p duduclaw-gateway migration_authority_profile_uses_normal_review_writer_and_live_registry
cargo test -p duduclaw-gateway migration_authority_task_schema_matches_fresh_store
cargo test -p duduclaw-gateway migration_authority_shared_label_never_substitutes_for_principal_readback
cargo test -p duduclaw-gateway migration_authority_same_content_replacement_is_not_owned_or_compensated
cargo test -p duduclaw-gateway migration_authority_config_refuses_equal_normal_publication_and_compensation
cargo test -p duduclaw-gateway migration_authority_config_receipt_survives_restart_and_rejects_google_source_change
cargo test -p duduclaw-gateway migration_import_schema_catalog_matches_approval_channel_and_cron_stores
```

Run privileged tests only in a disposable native Linux VM. The importer must
already be installed as a root-owned executable with root-owned non-writable
ancestors outside `/home`; its sandbox hides `/home`. Optional default protected
paths may be absent; the pipeline also hides the actual fresh home and quarantine.
Do not run these against a customer installation.
Repeat the native matrix on Ubuntu 22.04, Ubuntu 24.04 and Debian 12:

```sh
sudo env DUDUCLAW_IMPORT_TEST_BINARY=/usr/lib/duduclaw/current/duduclaw-import cargo test -p duduclaw-migration -- --ignored iso_root_ --test-threads=1
sudo cargo test -p duduclaw-gateway -- --ignored iso_root_migration_import_ --test-threads=1
```

The migration root filter covers DynamicUser FD transport, SQLite+WAL transport,
parser rejection versus isolation failure, actual acceptor credentials behind a
root-created socket, snapshot/rotation resume, pidfd stopping, transient unit
inventory and blob-copy constraints. These tests do not replace a signed F/B/H
installation plus full legacy-upgrade acceptance.

When web dependencies are available:

```sh
cd web
npx tsc -b
npx vitest run src/pages/MigrationConfirmationsPage.test.tsx src/components/layout/nav-model.test.ts src/components/FirstRunGate.test.tsx
```

Round 3 file responsibilities (rounds 1–2 remain uncommitted in this worktree):

| Files | Change |
| --- | --- |
| `crates/duduclaw-migration/src/import_launcher.rs` | Verified DynamicUser launch, bounded output and read-only descriptor handoff |
| `crates/duduclaw-migration/src/root_sources.rs` | Pinned no-follow source inventory and exact source routing |
| `crates/duduclaw-migration/src/root_pipeline.rs` | Root import coordinator and explicit installed platform boundary |
| `crates/duduclaw-migration/src/import_manifest.rs` | Private bounded root manifest with inert authority proposals |
| `crates/duduclaw-migration/src/import_receiver.rs` | Live acceptor credential handshake and staged relay |
| `crates/duduclaw-migration/src/{root_host,native_stop,journal,lib}.rs` | Composed pipeline access, pending-admin outcome and evidence/error plumbing |
| `crates/duduclaw-migration/src/bin/duduclaw-migrate.rs` | Select composed installed integration |
| `crates/duduclaw-import/src/{authority,lib,linux,main,sqlite}.rs` | Authority proposal extraction, FD worker mode, parser exit status and defensive SQLite handling |
| `crates/duduclaw-import/src/*-v1793.sql`, `scripts/capture_*_schema.py` | Reviewed exact authority database schema fixtures |
| `crates/duduclaw-import/tests/linux_binary.rs`, `systemd/duduclaw-import@.service` | Worker regression and sandbox template updates |
| `crates/duduclaw-gateway/src/migration_startup.rs` | Exclusive hardened import-only boot |
| `crates/duduclaw-gateway/src/migration_import.rs` | Inert archive imports, status/readback and durable receipts |
| `crates/duduclaw-gateway/src/{lib,server}.rs`, `crates/duduclaw-cli/src/lib.rs` | Early startup branch before ordinary initialization |
| `crates/duduclaw-cli/tests/migration_import_startup.rs` | Real CLI subprocess refusal before logging/configuration writes |
| `crates/duduclaw-gateway/src/handlers/migration_authority*.rs` | Inert staging, normal MCP/profile writers, Shared proof and planted legacy tests |
| `crates/duduclaw-gateway/src/handlers/{migration_confirmations_rpc,mod}.rs` | Live context wiring and honest per-item availability |
| `crates/duduclaw-auth/src/{migration,db}.rs` | Atomic Shared binding receipts using normal binding semantics |
| `Cargo.lock`, importer/gateway `Cargo.toml` | Existing TOML dependency and test-only importer dependency |
| Migration/importer `README.md`, this handoff | Current implementation, limits and exact verification commands |
| `docs/features/*/05-security-defense.md`, `docs/guides/*/linux-protected-service.md`, English equivalents, `CHANGELOG.md` | Three-language evidence-bounded behavior notes |

### Root execution and recovery (historical round 2)

`duduclaw-migration::root_host::RootMigration` validates root-owned ancestors,
locks the installation across migration IDs, pins an immutable plan, and advances
the durable journal only after stage-specific readback. `RootMigrationIntegration`
is the named F/B/A/D/H integration seam. `preflight` defaults to refusal; normal
`run` refuses before stopping services if downstream integration is missing.

The explicit maintenance command `prepare --plan <root-owned-plan> --stop-after-keys`
stops at `KeysRotated` and leaves the installation OFFLINE. It is not a full
migration. It inventories installed and loaded/transient services, follows
activation triggers, masks units, locks the old account, disables linger and
user-manager sessions, checks cron/at ACLs and jobs, and kills/rechecks processes
through pidfds. Unit names support exact systemd hexadecimal escapes and reject
leading option syntax. Commands have deadlines and bounded captured output.

Quarantine uses same-filesystem RENAME_NOREPLACE and a root-only directory.
Snapshot walking uses pinned descriptors and openat2 with no symlink, magic-link
or mount traversal. Special files are metadata only; file identity, byte/file/depth
limits and bounded xattrs are checked. AEAD chunks bind migration/sequence/length;
terminal authentication prevents truncation and a failed writer cannot reuse a
nonce. Publication fsyncs and verifies decryption before publishing. Fresh home
creation and root/identity/JWT/internal-MCP key publication use actual product
formats and resumable root-owned staging. Fresh configuration disables dispatch
and host-credential inheritance and requires MFA.

`status`, `export-records` and `expire` are implemented. `new-setup-token` has a
root-terminal/stage-gated `RootBootstrapIssuer` seam, not a working issuer: this
worktree has no setup-token claim route. The adapter must use the gateway's
already-open fresh UserDb and return the actual product URL; root never opens
gateway-owned SQLite/WAL and never prints a fabricated link. Recovery exports inert
JSONL into a new root-owned directory, not a restored runnable installation.
Expiry uses the immutable deadline, verifies the snapshot before removing
plaintext, traverses without following links, then removes the encrypted archive
and its key. The supplied daily systemd expiry templates are not installed.
Cross-filesystem quarantine and reconstruction of a regular-file recovery tree
are still unsupported and refuse.

### Import worker and actual gateway writes

`duduclaw-import` is a non-root Linux fd worker. It refuses missing read-only
regular source descriptors, missing NoNewPrivileges and unknown input formats;
unrelated inherited descriptors are closed. The DynamicUser/no-network/resource
profile exists, but a real launch/property-readback/SCM_RIGHTS integration is still
required before trusting worker output.

Supported formats: inert UTF-8 content, attachments/work-product hashes, exact
fresh v1.79.3 sessions and memory SQLite schemas. The memory catalog is reconstructed
from reviewed engine schema SQL; its equality test against a real fresh engine is
written for the remote gateway builder and has not yet been run. Unknown legacy
schema variants reject the whole DB. SQLite parses only a private DB/WAL copy,
with defensive/trusted-schema/extension/trigger/view restrictions, a read allowlist,
query deadline, per-value/op limits and a global hard heap cap that only narrows.
No raw SQLite errors, schema names or input content enter diagnostic messages.

`import_receiver::StagedImport` checks the entire bounded stream and successful
worker exit before making records available. Migration, employee, source, kind,
sequence, record count and stream digest are host-bound. The root relay verifies
the destination gateway UID and checks completion readback; the gateway connection
handler requires root SO_PEERCRED. `ImportModeGate` must prove no public dashboard,
channels or employee execution are running and resolve the new immutable employee.
Its unavailable implementation refuses before opening destination databases.

`GatewayImportWriter` uses a durable content ledger for restart retries. Memory
writes use the normal engine API with origin trust 0 and import lineage; imported
content cannot supersede new higher-trust facts. Wiki writes use the normal wiki
API with old frontmatter quoted as inert JSON. SessionManager imports quoted
historical user content into archived sessions with message and receipt in one
transaction; system/tool roles, summaries, live channel bindings and wakeups are
not revived. Task/audit content stays in an inert archive outside live actions.

`import_blobs::copy_blob` implements root fd-relative content copy, a path
allowlist, O_PATH classification, no links/xattrs/mount traversal, quota/count/depth
limits, source identity and hash checks, private staging, fchown, atomic publication
and destination readback. B/H must provide confirmed allocation, durable quota and
root copy receipts; gateway refuses blob completion without them.

### Dashboard authentication and decisions

The Admin-only review RPC rechecks the live account and credential epoch. Password
plus enrolled TOTP establishes a five-minute proof tied to the verified dashboard
session. CSRF is random, persistent and session-bound. TOTP replay and password
attempt limits survive restart; demotion, credential changes, expiry and clock
rollback refuse authorization. No client-provided MFA/admin flags are accepted.

The page now supports individual approval/rejection, bulk approval of low-risk
proposals, atomic bulk rejection of untouched proposals, retry/resume after a
crash and credential replacement. Both edition nav lists retain the 1.80.0 badge.
Untrusted values are rendered as plain text, full high-risk fields are scrollable,
and fingerprints are masked. Unsupported items can be rejected but not approved.
The UI reloads readback state after mutations; an approval response is never
presented as proof that application succeeded.

Confirmation transactions bind operation ID, revision and complete normalized
proposal digest. Approved payloads cannot change. Stage/publish/readback/disable
steps remain durable and repeatable; failure is committed before compensation.
At most 200 employee proposals and 200 decisions per request are accepted. Current
bulk approval is conservatively limited to Department labels. Migration remains
incomplete until root completion readback, all proposals and secret obligations
are settled.

### Actual authority writers and secrets

`GatewayAuthority` implements normal Department and invitation-only User writes,
delegation-policy configuration and agent budgets. The invitation and operation
receipt are atomic; a duplicate email is never adopted as an idempotency receipt.
Compensation suspends the same account and fences its credential epoch; retry
cannot resurrect an account already activated with a password.

Configuration staging is encrypted, commits use config_commit compare-and-swap,
and compensation refuses to overwrite concurrent edits. Budget and delegation
are read from disk on each normal gate. Other configuration requires
`MigrationConfigReadback` to reload and verify effective runtime state. Without
that adapter it is not approvable and cannot become Applied merely because a file
matches. `MigrationAuthorityBackend` is the explicit per-category seam for the
remaining authority types; no default implementation grants authority.

Secret values stay outside proposal/audit/export schemas in encrypted staging.
Fingerprints use the same protected HMAC key when proposing and rotating. The
rotation path writes through the normal configuration writer, verifies effective
readback, and only then records the new fingerprint. The root proposal producer
still needs to connect fingerprint_secret/stage_legacy_secret; a client assertion
cannot clear rotation obligations. `GatewayAccountReadback` is now wired into the RPC for the narrow `accounts`
configuration proposal. It accepts one explicit API-key account using the product
provider allowlist, normal account-entry builder and encrypted credential writer.
Host credential inheritance must be explicitly false; OAuth, `[api]` fallback,
remote secret references and ambiguous pool IDs refuse. Readback invalidates and
rebuilds the normal rotator and checks exact account/provider/priority/budget and
selected plaintext key internally. Compensation reloads and verifies removal.
Rotation only changes the encrypted key field, preserving later account edits.
It makes no provider network call, does not prove provider-side validity, does not
revoke the old key at its provider, and does not terminate already-running work
that holds an older rotator reference, including an existing Discovery run. Two-provider real-rotator tests are written
but await the remote gateway builder.

### Remaining work and acceptance boundaries

- Signed installer/F integration for preflight and the later migration stages;
  actual DynamicUser launcher, source FD transport and sandbox-property readback.
- Import-only gateway boot/listener and authoritative migration manifest;
  external OAuth revocation/reconnect report and manual rotation inventory.
- Legacy TOML/JSON authority extraction and unsupported historical SQLite catalogs.
- Concrete Employee, Role, EmployeeInstructions, user/channel bindings, MCP,
  Skill, Hook, Connector, DataSourceAuthorization and Schedule backends; some old
  proposal shapes lack a target employee/artifact/source handle and cannot safely
  infer one. B/H confirmed UID/disk allocation and blob receipt integration.
- Personal owner fresh-login binding, first-admin service startup integration,
  final isolation/protocol/ownership/quota readback and completion reporting.
- Cross-filesystem quarantine, file-tree recovery and installed expiry timer.
- Three-distribution root acceptance, actual v1.79.3 upgrade, gateway/CLI build,
  web TypeScript/Vitest/browser checks and formal Daybreak security review.

Dependencies added to these small crates already existed in Cargo.lock (including
chrono/tempfile/libc/sha2); no heavy new dependency or shared contract change.

### Round 2 validation and handoff

Actual local results (2026-11-05):

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth --offline` | 135 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-import -p duduclaw-isolation --offline` | migration 31, importer 10, isolation 29; 70 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline` | PASS; type check only, not Linux execution; syscall unsafe lint warnings remain |
| Migration translation JSON and key parity | PASS, 85 matching keys in three languages |
| `git diff --check` | PASS |

Total: **205 distinct local tests passed**. Earlier implementation errors (a
password-hash field lookup, Linux read/type inference and FTS schema authorizer
restrictions) were corrected before these final checks. No gateway/CLI/workspace
build was run, as requested. `web/node_modules` does not exist, so TypeScript,
Vitest and browser checks were not run and dependencies were not installed.

Independent code review found and prompted fixes for ineffective config readback,
missing transient units, raw SQLite diagnostics, pre-validation relay reads,
SQLite heap limits and escaped unit names. Final static re-review confirmed that
credential rotation preserves later budget/priority/label/tag edits, partial-write
errors are described accurately, and zero-quota blob replay only accepts a verified
existing artifact. Gateway tests cover same-value retry after a committed secret
write with failed runtime readback; they are still remote-only, not executed here.
This is not a formal Daybreak review.

Remote builder commands (not run locally):

```sh
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-gateway migration_confirmation_
cargo test -p duduclaw-gateway migration_decisions_
cargo test -p duduclaw-gateway migration_authority_ -- --test-threads=1
cargo test -p duduclaw-gateway migration_import_
# Includes exact current MemoryEngine schema versus importer fixture comparison.
# The native tests below require disposable Linux VMs, not a production host.
cargo test -p duduclaw-migration root_host::
cargo test -p duduclaw-import --test linux_binary -- --nocapture
cargo test -p duduclaw-migration -- --ignored iso_root_
cargo test -p duduclaw-gateway -- --ignored iso_root_migration_import_
cd web
npx tsc -b
npx vitest run src/pages/MigrationConfirmationsPage.test.tsx src/components/layout/nav-model.test.ts src/components/FirstRunGate.test.tsx
```

The root test suite includes quarantine/snapshot/key-rotation resume and lock,
symlink/plan-change refusal, pidfd stop, transient delayed-restart inventory,
root blob-copy rejection/readback and the prior snapshot walker test. Run it on
Ubuntu 22.04, Ubuntu 24.04 and Debian 12 before claiming Linux acceptance.

Round 2 file responsibilities (in addition to the historical round 1 list):

| Files | Change |
| --- | --- |
| `crates/duduclaw-migration/src/root_host.rs` | Root plan, lock, journal runner, quarantine, snapshot, fresh keys, expiry and export |
| `crates/duduclaw-migration/src/native_stop.rs` | Actual service/activation/account/job/process lockdown and readback |
| `crates/duduclaw-migration/src/root_bootstrap.rs` | Root-terminal bootstrap issuer seam and safe output boundary |
| `crates/duduclaw-migration/src/bin/duduclaw-migrate.rs` | Root CLI run, explicit offline prepare, status, export, expiry, bootstrap entry |
| `crates/duduclaw-migration/systemd/duduclaw-migrate-expire@.service` | Uninstalled expiry service template |
| `crates/duduclaw-migration/systemd/duduclaw-migrate-expire@.timer` | Uninstalled daily expiry timer template |
| `crates/duduclaw-migration/src/import_receiver.rs` | Host-bound full-stream staging and root relay |
| `crates/duduclaw-migration/src/import_blobs.rs` | Root bounded no-follow file copier and readback |
| `crates/duduclaw-migration/src/confirmation.rs` | Existing-store writes, atomic bulk reject, authorization and rotation status |
| `crates/duduclaw-migration/src/confirmation_tests.rs` | Bulk rejection, missing database and existing decision regression coverage |
| `crates/duduclaw-migration/src/lib.rs` | New modules and unchanged contract re-exports |
| `crates/duduclaw-migration/Cargo.toml` | Existing-lockfile small dependencies |
| `crates/duduclaw-migration/README.md` | Real command usage and remaining integration boundaries |
| `crates/duduclaw-import/src/lib.rs` | Memory SQLite and bounded attachment/work-product source formats |
| `crates/duduclaw-import/src/sqlite.rs` | Exact memory catalog, heap cap and strict extraction |
| `crates/duduclaw-import/src/memory-v1793.sql` | Trusted fresh-engine schema fixture |
| `crates/duduclaw-import/scripts/capture_memory_schema.py` | Reproducible schema extraction from reviewed source |
| `crates/duduclaw-import/tests/content.rs` | Blob hash/binding rejection tests |
| `crates/duduclaw-import/tests/linux_binary.rs` | Non-root fd/NNP failure checks |
| `crates/duduclaw-import/Cargo.toml` | Existing sha2 dependency |
| `crates/duduclaw-import/README.md` | Supported formats and launcher boundary |
| `crates/duduclaw-gateway/src/migration_import.rs` | Root connection gate, real content writes and archive receipts |
| `crates/duduclaw-gateway/src/lib.rs` | Gateway import module registration |
| `crates/duduclaw-gateway/src/handlers/migration_confirmations_rpc.rs` | Live authentication, decisions, apply/resume and rotation RPC wiring |
| `crates/duduclaw-gateway/src/handlers/migration_authority.rs` | Durable product writers, compensation and encrypted secret handling |
| `crates/duduclaw-gateway/src/handlers/migration_authority_accounts.rs` | Encrypted account writes and effective rotator readback |
| `crates/duduclaw-gateway/src/handlers/mod.rs` | Protected namespace dispatch and module registration |
| `crates/duduclaw-auth/src/mfa.rs` | Persistent migration proof/attempt schema |
| `crates/duduclaw-auth/src/mfa/migration.rs` | Password/TOTP/session/epoch/CSRF authorization |
| `crates/duduclaw-auth/src/mfa/tests.rs` | Expiry, replay, demotion and restart rate-limit tests |
| `crates/duduclaw-auth/src/migration.rs` | Atomic invitation account/idempotency and compensation |
| `crates/duduclaw-auth/src/lib.rs` | Auth migration module export |
| `crates/duduclaw-cli/src/service/protected_linux_install.rs` | Cross-version refusal points to verified migration tooling |
| `web/src/lib/migration-confirmations.ts` | Typed decisions, step-up and credential replacement calls |
| `web/src/pages/MigrationConfirmationsPage.tsx` | Actionable authenticated individual/bulk/retry/rotation controls |
| `web/src/pages/MigrationConfirmationsPage.test.tsx` | Decision, authentication, rejection, masking and readback regressions |
| `web/src/i18n/{zh-TW,en,ja-JP}.json` | User-facing confirmation translations |
| `docs/features/{,zh-TW/,ja-JP/}05-security-defense.md` | Actual enabled authentication/confirmation behavior |
| `docs/guides/{,zh-TW/,ja-JP/}linux-protected-service.md` | Review workflow and migration acceptance limits |
| `CHANGELOG.md` | Self-contained unreleased user-facing behavior |
| `Cargo.lock` | Resolve only already-present dependency versions |
| `docs/architecture/native-linux-isolation.md` | Current implementation, tests, missing work and historical handoff |
| `wiki/reports/native-linux-isolation-package-g-2026-11-05.md` | Removed; its full historical text moved below, not left in published wiki |

### Historical round 1 handoff (superseded by round 2 below)

#### Package G 交付紀錄（2026-11-05）

狀態：**PARTIAL**。只修改交辦 worktree，沒有 commit、沒有建立或移除 worktree、沒有操作既有安裝或客戶資料。共用 contract 無變更。

本輪交付可測試的遷移元件、內容匯入器、首位管理員原子領取、唯讀確認頁與舊安裝器的跨版本拒絕。這不等於完成 Package G，也不能執行完整 v1.79.3 遷移。詳細介面與限制見 [實作說明](../../docs/architecture/native-linux-isolation.md)。

##### 實際驗證

| 指令 | 結果 |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-import -p duduclaw-isolation --offline` | PASS：migration 25、import 7、contract 29，共 61 passed / 0 failed；Linux 專屬測試在 macOS 未執行 |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth otp::tests --offline` | PASS：18 passed / 0 failed，包含兩條獨立 SQLite 連線搶同一 token |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline` | PASS：Linux Rust 原始碼及測試型別檢查，未連結或執行 Linux binary；使用 SQLite 既有 bindings，未編譯 bundled SQLite C |
| `cargo check --target x86_64-unknown-linux-gnu -p duduclaw-import --offline`（初次原配置） | FAIL：缺 `x86_64-linux-gnu-gcc`，停在 bundled SQLite C build；不算 Rust 原始碼失敗，也不算 Linux 實機驗收 |
| `git diff --check` | PASS |
| 三語 JSON 解析及 key 集合比較 | PASS |

合計實際執行 79 個不重複測試通過。早期 migration 測試因 journal 測試使用 enum 不支援的 struct update 語法失敗，修正後已由上述完整指令重跑通過。Importer 保留有說明的 unsafe syscall/SQLite FFI，workspace `unsafe_code=warn` 會印警告。

未跑 gateway／CLI／workspace build（依交辦資源限制）；未跑 web TypeScript／Vitest（`web/node_modules` 不存在，未安裝依賴）；未跑 Linux 原生、Windows、設計畫布或瀏覽器驗收。Linux 交叉型別檢查不能替代上述驗證。

##### 審查

獨立 code-reviewer 看過實際變更；指出的缺 version-floor 放行、proposal 總量上限及 inherited fd 的 unsafe precondition 已修正並複查。archive I/O 錯誤後禁止再次加密以避免 nonce 重用，另有回歸測試。沒有正式 `gpt-daybreak-blue-latest` 安全審查，不能視為 Daybreak GO。

##### 精確未完成項目

1. 尚無可操作的 `duduclaw-migrate` root 命令及 installer 遷移接線。`HostAdapter` 沒有 production implementation：root flock、停止舊 gateway、mask/timer/socket/cron/at/user-manager 封鎖、清空舊 uid 程序與讀回待做。舊 installer 目前只會拒絕跨入 v1.80。
2. 快照走訪／AEAD 元件已有，但 fsync＋原子發布＋解密驗證整合、人工安全還原、期限清理及跨檔案系統複製待做。quarantine helper 只有同檔案系統 `RENAME_NOREPLACE`，呼叫端須先停止舊環境並封存父目錄權限。
3. 新 gateway 空資料目錄、root／identity／MCP／JWT 金鑰實際輪替、外部 OAuth 撤銷、手動輪替報告與管理員 setup link root-only 輸出尚未接線；journal 只檢查可信 adapter 的證據。
4. Importer 有 systemd profile，但 root 啟動／fd 交付／實際 sandbox 屬性讀回未接線。只支援 inert text 與精確 fresh v1.79.3 sessions.db schema；memory.db、其他歷史 schema、TOML／JSON 權威提案抽取、附件與工作產出複製待做。
5. root-only gateway import endpoint、完整 stream 暫存到 worker 成功結束、host-bound migration/employee/source handle 檢查、正常 content API／最低信任 lineage 寫入未接線。Importer stdout 不可直接當成功完整匯入。
6. 真實 session CSRF／step-up provider、每一類權威提案的正常寫入驗證／stage／publish／readback／disable adapter、secret vault／rotation verifier 未接線。因此 UI 只讀，所有 mutation RPC 拒絕；目前只有 Department 可批次核准的 engine 策略也較設計保守。
7. 舊員工名稱對 immutable id、確認後 UID／loop 配置與容量檢查、舊使用者邀請、Personal owner 重新登入綁定、部分套用資源 tombstone／清除等，都仍是必須實作的 product adapters。
8. 三個 Linux 發行版的原生／root 測試、完整 legacy planted authority 端到端遷移、gateway／CLI build、web build/tests/視覺驗收與正式 Daybreak 審查待做。

##### 下一位執行者的驗證指令

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-import --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth otp::tests --offline
#### Linux 非 root，實際驗證缺少來源 fd 的拒絕（root 會在更早的身分檢查拒絕）
cargo test -p duduclaw-import --test linux_binary -- --nocapture
#### Linux root，可拋棄測試機
cargo test -p duduclaw-migration -- --ignored iso_root_
#### 遠端 builder：本地明確未跑
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-gateway migration_confirmation_methods_are_exact
cd web
npx tsc -b
npx vitest run src/pages/MigrationConfirmationsPage.test.tsx src/components/layout/nav-model.test.ts src/components/FirstRunGate.test.tsx
```

##### 依賴及契約

無 CONTRACT CHANGE。兩個新 workspace crates 使用 lockfile 已有的 rusqlite、serde/json、thiserror、sha2、hmac、chacha20poly1305、rand、rustix、libc、tempfile；沒有新增重型套件。SQLite worker 新增既有 rusqlite 的 hooks/limits/modern_sqlite feature。

##### 檔案清單

- `CHANGELOG.md`：未發行功能、認證與舊升級拒絕紀錄。
- `Cargo.lock`：登錄新 crates 的既有依賴。
- `Cargo.toml`：加入兩個新 workspace members。
- `crates/duduclaw-auth/src/db.rs`：setup token 用途／嘗試次數 schema 與舊 token 撤銷。
- `crates/duduclaw-auth/src/otp.rs`：原子 claim、期限／速率／用途區分及 18 項測試。
- `crates/duduclaw-cli/src/service/protected_linux_install.rs`：阻止缺失紀錄或舊版直接跨入 v1.80。
- `crates/duduclaw-gateway/Cargo.toml`：加入 migration engine 依賴。
- `crates/duduclaw-gateway/src/handlers/migration_confirmations_rpc.rs`：Admin／live session 唯讀清單及 mutation 拒絕。
- `crates/duduclaw-gateway/src/handlers/mod.rs`：攔截精確 migration RPC 名稱，禁止 extension 接管。
- `crates/duduclaw-import/Cargo.toml`：內容匯入 crate／binary 與 Linux 依賴。
- `crates/duduclaw-import/README.md`：fd 協定、支援 schema 與整合限制。
- `crates/duduclaw-import/src/lib.rs`：有界請求、內容類型與 contract JSONL 輸出。
- `crates/duduclaw-import/src/linux.rs`：來源 fd 與 worker 身分／NNP／dumpable 檢查。
- `crates/duduclaw-import/src/main.rs`：固定 fd 啟動、額外 fd 清理、非 Linux 拒絕。
- `crates/duduclaw-import/src/session-v1793.sql`：精確可信 sessions schema。
- `crates/duduclaw-import/src/sqlite.rs`：私有 DB/WAL 副本、SQLite 防禦與 schema 驗證。
- `crates/duduclaw-import/systemd/duduclaw-import@.service`：未安裝的 DynamicUser 無網路限制 profile。
- `crates/duduclaw-import/tests/content.rs`：內容-only／限額／未知輸入拒絕測試。
- `crates/duduclaw-import/tests/linux_binary.rs`：非 root Linux 缺 fd 子程序拒絕測試。
- `crates/duduclaw-migration/Cargo.toml`：遷移元件 crate 依賴。
- `crates/duduclaw-migration/README.md`：元件使用及尚未啟用界線。
- `crates/duduclaw-migration/src/archive.rs`：分塊 AEAD、結尾驗證與失敗後禁止 nonce 重用。
- `crates/duduclaw-migration/src/confirmation.rs`：原子核准、冪等套用／補償／輪替及容量限制。
- `crates/duduclaw-migration/src/confirmation_tests.rs`：17 項核准、故障、重試與秘密測試。
- `crates/duduclaw-migration/src/journal.rs`：durable intent／readback／receipt 及逐階段中斷測試。
- `crates/duduclaw-migration/src/lib.rs`：模組與不含秘密的錯誤類型。
- `crates/duduclaw-migration/src/snapshot.rs`：Linux no-follow 有界 walker、digest 與同檔案系統隔離。
- `docs/README.md`：新增架構文件索引。
- `docs/architecture/native-linux-isolation.md`：Package G 實作與缺口的自成段說明。
- `docs/features/05-security-defense.md`：說明已啟用的 setup token 修正與遷移只讀界線。
- `docs/features/ja-JP/05-security-defense.md`：說明已啟用的 setup token 修正與遷移只讀界線。
- `docs/features/zh-TW/05-security-defense.md`：說明已啟用的 setup token 修正與遷移只讀界線。
- `docs/guides/ja-JP/linux-protected-service.md`：說明舊安裝器拒絕跨版與尚未接線的遷移。
- `docs/guides/linux-protected-service.md`：說明舊安裝器拒絕跨版與尚未接線的遷移。
- `docs/guides/zh-TW/linux-protected-service.md`：說明舊安裝器拒絕跨版與尚未接線的遷移。
- `web/src/App.tsx`：註冊管理員遷移確認頁。
- `web/src/components/FirstRunGate.test.tsx`：零員工導向回歸測試。
- `web/src/components/FirstRunGate.tsx`：未建立員工時仍可開遷移與帳號安全頁。
- `web/src/components/layout/nav-model.test.ts`：兩種導覽可見性及 badge 測試。
- `web/src/components/layout/nav-model.ts`：兩種 edition 共用導覽加入 newIn 1.80.0。
- `web/src/i18n/en.json`：遷移頁與欄位名稱翻譯。
- `web/src/i18n/ja-JP.json`：遷移頁與欄位名稱翻譯。
- `web/src/i18n/zh-TW.json`：遷移頁與欄位名稱翻譯。
- `web/src/lib/migration-confirmations.ts`：typed RPC request 與選取規則。
- `web/src/pages/MigrationConfirmationsPage.test.tsx`：高風險／未就緒／遮罩／讀回失敗測試。
- `web/src/pages/MigrationConfirmationsPage.tsx`：純文字檢視／秘密遮罩／唯讀拒絕 UI。
- `wiki/reports/native-linux-isolation-package-g-2026-11-05.md`：交付證據、完整剩餘工作與遠端驗證指令。
