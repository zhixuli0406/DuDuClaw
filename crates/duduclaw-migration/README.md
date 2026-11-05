# duduclaw-migration

Migration building blocks for moving untrusted v1.79.3 data into the planned
native Linux isolation installation. Content and authority have separate paths:
the importer emits bounded content, and authority stays inactive until a fresh
administrator approves and the normal product writer reads back its effects.

The root journal records intent before each side effect and advances only after
readback. The confirmation store separates atomic approval from resumable apply,
including disable compensation. Linux snapshot operations use directory file
descriptors and never follow links. Shared wire types remain in
`duduclaw-isolation`. The root journal now distinguishes `ContentArchived` from
post-confirmation `ContentImported`; archive-only receipts do not prove import.

## Integration status

The Linux binary `duduclaw-migrate` owns a root lock and durable journal, validates
an immutable root-owned plan, and implements stopped-environment readback,
quarantine, verified encrypted snapshot publication and four fresh key products.
The native stop path masks the old service and associated user services/timers/
sockets, disables login and linger, blocks cron/at access, removes queued user jobs,
refuses explicit system cron references, and uses pidfds to stop remaining uid
processes. Unknown scheduler or service state refuses migration.

`run --plan /root/migration.json` now composes source inventory, the DynamicUser
launcher, root-owned import manifests, an exclusive gateway service/socket and
complete-stream/confirmation readback. It returns `AwaitingAdministrator` while
human decisions are pending, and never treats that state as complete.

The final F/B/H seam is `root_host::pipeline::MigrationPlatform`. The CLI's
`InstalledPlatform` expects a signed, root-owned
`/usr/lib/duduclaw/current/duduclaw-migration-platform` executable. **That executable
is not supplied here.** Its fixed `migration-v1` command accepts bounded JSON
`{version:1, action, operation, plan}` on stdin and returns
`{version:1, ready, evidence}` on stdout. Actions are `preflight`, `perform` and
`readback`; evidence uses the typed journal schema and must reflect live state.
Preflight must verify the signed generation and B/H allocation/readiness before
any old service is stopped. Later operations cover IsolationVerified,
AwaitingAdministrator and Complete. `ContentImported` perform must stop the
ordinary gateway and all employee execution for the materialization batch. The
coordinator subsequently calls AwaitingAdministrator perform/readback to restore
the dashboard, including after a partial batch. A missing adapter refuses before outage;
this is still not a complete deployable migration command.
Preflight also rejects a fresh home under `/home`, `/root` or `/run/user`, which
the import service's `ProtectHome=yes` would mask. Actual fresh-home and
quarantine paths are passed to each import worker's `InaccessiblePaths`; no
default-directory existence is assumed.

## Post-confirmation content and recovery

On each root `run` resume, confirmed employees can materialize independently of
pending employees. The additional fixed platform action `materialization-allocation`
accepts `{version:1, action, plan, proposal}` and returns
`{version:1, allocation, stopped:true, quota_verified:true}`. `allocation` contains
`proposal` (operation ID and digest), immutable `employee`, `agent`, `uid`,
`allocation_digest`, `quota_bytes`, and the absolute `workspace`. It must come from
live registry/mount/quota and stopped-process readback. Neither an Applied proposal
nor an existing employee directory is allocation proof. The signed platform and
production Employee authority backend remain integration requirements; neither is
implemented by this crate.

Root commits the allocation intent before copying any blob and records each copy
and readback. The gateway records every authenticated record in
`migration-materialization.db`, commits its exact allocation-bound intent, writes
to real memory/wiki/session stores, verifies the result, then CASes its terminal
receipt. Attachments are copied to the gateway attachment store; work products go
to the confirmed employee workspace. Stable operation IDs and receipts make
interrupted publication resumable without duplicating content. Completed streams
are not recopied during a later employee's batch. History/audit records have an
explicit administrator-read-only retention policy.

Rejected employee records are displayed as “not migrated (rejected)”; plaintext
archive payloads are securely deleted and WAL is truncated before rejection
receipts become terminal. The encrypted snapshot retains the original until its
deadline. `ContentImported` and `Complete` require terminal receipts for every
accepted record. Missing receipts refuse cleanup even after expiry.

Old archive-only journal receipts are reclassified as ContentArchived, including
rewinding an old Complete checkpoint to AwaitingAdministrator. Old source plans
that lack the root-owned `content-inventory-v2.json` seal cannot materialize or
clean up: their inventory omitted attachments/work products. They require a
trusted new inventory/recovery procedure; this patch deliberately does not invent
missing receipts or silently delete their snapshot.

Confirmation adapters commit durable intents before external calls and CAS their
receipts afterwards. No adapter or fresh validator runs under a SQLite writer
transaction. The shared 30-second bridge and outer adapter deadline also bound
nested product file-lock waits; timeouts leave failed, compensable operations.

For an operator who deliberately wants an offline maintenance checkpoint,
`prepare --plan /root/migration.json --stop-after-keys` runs the implemented early
stages and leaves the installation **offline** at `KeysRotated`. This command does
not finish a migration or automatically restart the old service. The plan has
`migration` (16-byte JSON array), `state_dir`, `legacy_home`, `root_key_path`,
`gateway_uid`, `gateway_gid`, `snapshot_content_limit`, `snapshot_encoded_limit`
and `expires_at_unix`; it must be root-owned mode 0600 under root-controlled
ancestors. Destination parent directories must already exist. The signed installer
is responsible for constructing the plan from trusted installation metadata.

`new-setup-token --plan ...` is root-terminal-only and restricted to
`IsolationVerified`/`AwaitingAdministrator`. `GatewayBootstrapIssuer` connects to
`.migration-bootstrap.sock` in the fixed fresh home, checks the socket's ownership
and mode and authenticates the gateway UID/GID with `SO_PEERCRED`. The gateway only
accepts root peers and the current completed import manifest's migration ID. It
uses its already open `UserDb::issue_setup_token(false)`; root never opens the
user database. The response is bounded and the secret is never logged.

The real `/api/migration/setup` page receives the token in a URL fragment, removes
it from browser history, and claims it through a same-origin, loopback-only JSON
route. Token consumption, creating the first Admin and setting the chosen password
share one transaction; recovery tokens, replay and a nonempty user table refuse.
The gateway skips its ordinary default-admin and pending-account bootstrap for a
root-authenticated completed import. The root coordinator persists
`/etc/duduclaw/migration-import.json` before stopping the old installation and
before publishing the matching `/run` copy. Losing `/run` at reboot retains this
fence; missing or mismatching durable state refuses ordinary startup. This initial route requires the gateway to
bind `127.0.0.1` or `0.0.0.0`; remote operators use an SSH tunnel to the printed
loopback port. The static page has Traditional Chinese, English and Japanese text.
This wiring does not replace the still-missing signed `MigrationPlatform`, and
end-to-end migration and browser/native-socket acceptance remain unverified.

The protected CLI installer/upgrade now routes a version crossing into 1.80 to
G's transaction using the root-owned mode-0600 plan at
`/var/lib/duduclaw-migrate/plan.json`, before any legacy install or updater request.
A missing plan or platform refuses; it never copies old authority in place.
The installer accepts only the protected v1.79.3 home and root-key locations.
An `AwaitingAdministrator` checkpoint prints a newly issued link to the root
terminal and returns without claiming migration completion.

`status --plan ...` reports the persisted checkpoint. `export-records --plan ...
--destination /root/new-recovery` writes authenticated, inert recovery JSONL into a
fresh private directory; it never recreates a legacy filesystem or applies old
authority. When all proposals and per-record receipts are terminal, root verifies
the snapshot and immediately removes plaintext `legacy` and extracted `streams`
using a bounded no-follow/no-cross-mount walker. `expire --plan ...` uses the same
gate and deletes the encrypted snapshot/key at the fixed deadline; unresolved
records refuse deletion regardless of the deadline. Retry is safe after any of
those deletions. KeysRotated readback persists non-secret key digests, atomically
renames `fresh-keys`, and erases its secret copies; crash recovery never regenerates
keys after that receipt. Supplied expiry timers are not installed automatically.

The shared contract and existing product validators still govern imports and
confirmation. Do not enable isolated execution based solely on successful unit
tests or a persisted approval. Cross-filesystem quarantine remains refused.

## Validation

Run focused tests with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p
duduclaw-migration --offline`. Linux-only tests require a Linux target; tests
prefixed `iso_root_` are ignored and need a disposable root-capable host.

See [implementation notes](../../docs/architecture/native-linux-isolation.md).

## License

Apache-2.0, as declared by the workspace.
