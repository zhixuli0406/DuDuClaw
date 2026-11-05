# duduclaw-migration

Migration building blocks for moving untrusted v1.79.3 data into the planned
native Linux isolation installation. Content and authority have separate paths:
the importer emits bounded content, and authority stays inactive until a fresh
administrator approves and the normal product writer reads back its effects.

The root journal records intent before each side effect and advances only after
readback. The confirmation store separates atomic approval from resumable apply,
including disable compensation. Linux snapshot operations use directory file
descriptors and never follow links. Shared wire types remain in
`duduclaw-isolation`; this crate does not change their format or limits.

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
AwaitingAdministrator and Complete. A missing adapter refuses before outage;
this is still not a complete deployable migration command.
Preflight also rejects a fresh home under `/home`, `/root` or `/run/user`, which
the import service's `ProtectHome=yes` would mask. Actual fresh-home and
quarantine paths are passed to each import worker's `InaccessiblePaths`; no
default-directory existence is assumed.

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
`IsolationVerified`/`AwaitingAdministrator`. Its `RootBootstrapIssuer` adapter must
call the fresh gateway's already open `UserDb::issue_setup_token(false)` and
construct the link using the actual dashboard claim route. This worktree contains
no dashboard setup-token claim route, so the bundled adapter refuses: no guessed
URL is printed and root never opens gateway-owned SQLite/WAL files. Issuer wiring
is still required before this command can create a token.

`status --plan ...` reports the persisted checkpoint. `export-records --plan ...
--destination /root/new-recovery` writes authenticated, inert recovery JSONL into a
fresh private directory; it never recreates a legacy filesystem or applies old
authority. `expire --plan ...` verifies the snapshot before deleting plaintext
with a bounded no-follow/no-cross-mount walker, then removes the expired encrypted
snapshot and archive key. Retry is safe after any of those deletions. Supplied
systemd expiry templates are not installed automatically.

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
