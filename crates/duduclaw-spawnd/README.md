# duduclaw-spawnd

Root-owned launch coordination for the planned native Linux employee isolation.
The daemon accepts only the gateway account on the fixed seqpacket socket and
serializes capability-holding runs for each employee. It never executes employee
code itself or falls back to the gateway identity.

This package is an integration component, not a complete protected installation.
The production Host now installs the native B1/B2 transport adapter: it prepares
root-authorized descriptors, sends a sealed, nonce/run-bound manifest to
`duduclaw-run`, and consumes the bootstrap status protocol. It also includes root
account recovery, durable UID/run/allocation journals, real systemd transient
units, detached mount/executable preparation, bounded lifecycle jobs and an
independent intrusion monitor. A missing trusted runtime publication, verified
installation/readiness producer, DNS evidence or production storage setup still
refuses launches; installing the units does not enable employee isolation.

Protected Linux TEAM gateway boot selects SpawndBackend and applies the composite
admission gate to subprocesses and API-only model calls. Every required component
must be ready. The dashboard remains available while missing integrations keep
employee execution closed. This wiring is not production acceptance.
See [Package B1](../../docs/architecture/native-linux-isolation.md#package-b1)
for adapter contracts and remaining cross-package acceptance.

## Build and verification

Only Linux runs the service. The policy and admission tests are portable.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --target x86_64-unknown-linux-gnu --offline
```

Privileged tests are opt-in with the `iso_root_` prefix; run them only in a
disposable Linux VM. The service accepts no paths, argv, environment, uid or
systemd properties from its caller. `--check` exits unsuccessfully if any required
integration/readback is missing. It prints the root-owned readiness evidence from
`/run/duduclaw-spawnd/readiness.json`; boot identity and a one-second expiry prevent
stale evidence from authorizing a launch. It never treats process liveness as readiness.

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1
```

Those privileged tests are compiled but have not been run in this worktree.
They require a disposable root Linux VM and the installation prerequisites named
in each ignore reason. The account test modifies login ACLs; the systemd resource
test requires the complete trusted service hierarchy.

## Package H adapter

`storage::LoopStorage::open(storage_directory, journal_directory)` implements the
B1 storage seam with `duduclaw-isolation-storage::EmployeeStorage`. Both paths are
root installation inputs, not gateway request fields. The journal directory is a
separate root:root 0700 directory outside H's image directory (H rejects unknown
files). A lifetime flock protects the B1 journal; H separately locks its images.
The daemon now calls `Components::installed()`: it attempts the real H adapter at
fixed root installation paths `/var/lib/duduclaw-spawnd/images` (H images) and
`/var/lib/duduclaw-spawnd/storage` (B1 journal). There is no environment, config or
RPC override. The installer must precreate both root:root 0700 directories and
provide a private image mount tree on H-supported ext4/XFS backing with the
required capacity reserve. Boot does not create/mount those anchors or provision
an employee image; it only opens verified existing anchors and their metadata.
Any failure retains UnavailableStorage. The generic `Components::default()` stays
refusing for explicit/incomplete compositions. Installation evidence and root
runtime publication remain missing, so this does not enable TEAM execution.

Provision and destroy bind the complete proposal digest to the operation id.
Employee/UID tombstones remain after destruction. Run attachment intent and a
fresh generation token are fsynced before H mounts anything; after restart B1 can
recover that exact run even when attach failed before returning a receipt. A
stale receipt cannot detach a newer generation. Unknown images, missing Ready
images, unleased mounts and lost/invalid ledgers refuse readiness. Incomplete
maintenance remains fenced. The same approved operation can recover a completed
H image's temporary mount; H Creating/Quarantined images are not automatically
resumed and require explicit destruction or operator data recovery. Any failed journal publication poisons the current adapter.

H's `images()` reports kernel-verified image/mount identities for reconciliation.
Before reporting usage the adapter calls H readiness and reads real `statvfs`;
when idle it briefly mounts and detaches under a durable maintenance phase.
**CONTRACT CHANGE (I1b round 5):** StartRun optionally carries a bounded
`workspace` selector and one trailing directory fd after its existing prompt
and attachments (66 descriptors maximum; the 64 attachment cap is unchanged).
The fd pins D's AuthorizedWorkspace directory while queued. Root never mounts
that supplied fd: after H attachment it reopens only `employee-work` or
`.forks/<32 lowercase hex>` plus a validated relative cwd under the H work
mount using openat2 BENEATH/NO_SYMLINKS/NO_MAGICLINKS/NO_XDEV. Employee UID,
H device and authorization device/inode must all match. The reopened directory
becomes the existing detached `/work` mount, narrowing a nonempty cwd to that
subdirectory; the B2 bootstrap schema remains unchanged. Missing selectors retain
the original work mount. Old roots reject new fields; mixed versions fail closed.
Native root mount/queue acceptance is still required.

Both home and workspace are conservatively noexec. Only private detached clones
of their directories reach B1/B2, never images or block devices. B1 still owns the
unit/cgroup/UID-empty proof and must release every clone before detach/destroy.

The ignored `iso_root_storage_adapter_reopen_generation_and_proposal_binding`
test uses real loop storage and checks reopening, stale-token refusal and proposal
binding. It needs the same `ISO_STORAGE_TEST_ROOT`, private mount tree, ext4/XFS
backing and host capacity reserve as package H's root tests. It has not been run
in this worktree; it is not an end-to-end employee launch test.

## Landlock evidence and logging

B2's probe returns the detected kernel ABI and the selected ruleset ABI without
printing them to employee stderr. The B1 readiness interface carries both fields;
when a trusted installation producer returns verified evidence, the daemon logs
the pair once per daemon process to stderr and publishes it as `EvidenceReport.landlock`
(`detected_abi`, `selected_abi`). The supplied service currently discards stderr;
readiness is the retained operator evidence, and a bounded daemon-log destination
remains an installer task. The selected ABI cannot exceed 7. The default
native adapter still uses `MissingInstallation`, so the production producer is
absent: neither kernel probing nor an arbitrary configuration flag makes this
readiness evidence available. The bootstrap receipt's selected ABI alone is not
an installation self-test or an operator readiness report.

## UserDB account verification

Account checks enumerate Varlink providers directly; `userdbctl` is not required.
Every provider also receives named `GetUserRecord`/`GetGroupRecord` lookups
(`userName`/`groupName`) and numeric lookups (`uid`/`gid`) for the account being
checked or created, all registry employees when that list is available, and local
employee user/group entries. Each request includes the socket's `service`.
This catches providers that synthesize reserved aliases only on named lookup,
as permitted by the [user-record specification](https://github.com/systemd/systemd/blob/main/docs/USER_RECORD.md).
Only `io.systemd.UserDatabase.NoRecordFound` means no hit. Every targeted hit
must be an exact local NSS echo matching the queried name or ID; non-NSS hits,
other errors, timeouts and malformed replies are refused using the same strict
canonical parser and provider validation as enumeration.
Each record retains its socket provider and complete JSON until validation, with
duplicate JSON keys refused. Before merging identity pairs, any non-NSS provider
(including PID1's DynamicUser) touching a reserved UID/GID, a `duduclaw-emp-*`
name or alias, or a local employee account/group name is refused; unrelated
non-NSS identities remain allowed. Employee membership mappings are also protected.
`aliases` must be an array of strings wherever encountered, including top-level,
`status.*`, `perMachine[*]` and `binding.*` objects in user and group records.
The user-record specification defines aliases at top-level and in `status.*`;
the other locations and group records are checked conservatively.
NSS echoes touching this namespace must match local `/etc/passwd`, `/etc/group` and
`/etc/shadow` identity, home, shell, account expiry, password state and the
`members`/`memberOf` group mappings in these records. The separate `GetMemberships`
API is not queried. Employee accounts/groups have no aliases, so an NSS echo
carrying an alias field (even an empty array) is refused. Relevant incomplete
records and unsupported security attributes fail closed.
Recovery compares the current local state so a matching half-created account can
still be locked down; the normal admission checks continue to require lockdown.

An absent `/run/systemd/userdb` is allowed. An existing directory requires at
least one provider to complete both user and group enumeration and all targeted
lookups, including explicit
`NoRecordFound` replies. A DynamicUser-only installation satisfies this condition;
an empty or Multiplexer-only directory is refused. There is no Multiplexer
fallback. The 5-second total deadline, 8 MiB/65,536-record budgets, 256-entry limit,
fixed alias restrictions, root-owned socket and root peer checks remain enforced.
Enumeration and all targeted lookups across providers share those total budgets;
DynamicUser may answer non-dynamic employee lookups with `NoRecordFound`.
The NSS shadow mapping follows systemd's
[user-record-nss.c](https://github.com/systemd/systemd/blob/v252/src/shared/user-record-nss.c);
`service` can appear under `status[machine-id]` in
[v249](https://github.com/systemd/systemd/blob/v249/src/userdb/userwork.c),
[v252](https://github.com/systemd/systemd/blob/v252/src/userdb/userwork.c) and
[v255](https://github.com/systemd/systemd/blob/v255/src/userdb/userwork.c), so it is
checked as metadata, never used to override the queried provider.

These review fixes have portable regression tests. The earlier root-account
acceptance on Ubuntu 22.04/24.04 and Debian 12 must be repeated with this patch;
compilation and fixture tests do not establish live-provider acceptance.

### I1 round 3: installed publication consumers

`Components::installed` now consumes root publication through `installed.rs`.
F must have a verified committed generation containing an immutable mode-0444
`RuntimeEntry { registry_id: "isolation-policy-v1" }` artifact. Its strict JSON
contains `version: 1`, the bounded runtime table and architecture-specific native
profiles. The root-only `/var/lib/duduclaw-spawnd/publication.json` contains
`version`, the same `generation` digest and administrator-confirmed employee
entitlements. Paths are fixed, not supplied by a gateway request. This is a new
internal root publication schema; the shared IPC/capability contract is unchanged.

The verified generation directory is the runtime anchor. The installer must also
create root-owned `etc-state` (0700), `projections` and `snapshots` directories
under `/var/lib/duduclaw-spawnd`. Runtime entries must name the exact signed
artifact filenames visible in the generation. Publication emits a root-owned
0640 gateway projection at `/run/duduclaw-spawnd/publication.json`, bounded to
64 KiB and one second of monotonic validity. `GatewayPublication::read(gid)` checks
ownership/mode, boot ID, freshness and root-policy validity. No root private
state or executable path is disclosed through the projection.

`/run/duduclaw-isolation-evidence/installation.json` is a root-harness receipt,
not an operator configuration. It binds the committed generation and current
boot with at most 60 seconds validity. It records UID inventory and permanent
external-IAM reservation digests; per-account SSH positive control, denied
public-key login and forwarding observations; actual trampoline Landlock and
namespace test exit results; and DNS query/answer digests through resolved
Varlink or a dedicated broker with no private-IP allow exception. The consumer
also hashes the exact `/usr/lib/duduclaw/duduclaw-run` executable against F's
signed artifact, and repeats the live kernel ABI probe. Missing, stale, changed
or malformed receipts deny admission. The trusted root harness must renew real
observations; this change does not implement that harness or claim an installed
host is ready. Root caches only a fully verified immutable F generation and its
manifest. Every consumption still makes an authenticated nonce probe and checks
generation, product, exact binary version and all eight protocols; failed probes
clear the cache, and generation changes trigger full artifact verification again.
Freshness starts before the probe. A slow cold verification may warm the cache
but cannot publish readiness until a later fresh probe succeeds. Per-run runtime,
bridge and forwarder fd hashing is unchanged. Each `InstalledPublication`
instance pins the first verified generation for its entire daemon lifetime.
Load, mount anchors, MCP preparation/readback and native-profile selection all
refuse a different generation; F activation must restart B1. They compare the
complete supplied runtime against the signed record (including executable,
arguments, resource/time limits, network and MCP/PTY flags), and MCP preparation
and readback rebuild and compare the bootstrap plan before using its descriptors.
Native-profile selection revalidates the signed launch-policy acceptance. Thus a
new native policy cannot be mixed into an older plan between preparation stages.
The gateway publication is limited
to 64 KiB after encoding; larger runtime/entitlement sets refuse rather than
truncate, even when individual table bounds would permit them.

Linux builder checks and focused tests:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --all-targets
cargo test -p duduclaw-spawnd installed::tests -- --test-threads=1
# Disposable root VM; receipt transport only, not employee/runtime acceptance:
cargo test -p duduclaw-spawnd iso_root_e2e_publication_receipt_mode_and_symlink_refusal -- --ignored --test-threads=1
```

For an MCP-enabled installed runtime, the signed runtime record also requires
`mcp_acceptance`. It binds exact runtime/version, runtime and bridge hashes, D's
projection digest, fd 3, successful real-runtime initialization/parent exec/MCP
child exec/exclusive-config tests and a transcript digest. `launch_policy_digest`
is SHA-256 of the JSON object with domain
`duduclaw-runtime-mcp-acceptance-launch-v1`, `runtime` (the full `RuntimeRecord`
with `mcp_acceptance: null`) and `native` (the exact native profile). Changing
arguments, limits, sandbox profile or forwarder invalidates acceptance.

Before preparing detached mounts, the installed publication atomically writes
D's exact configuration bytes as root-owned mode-0444 files in a 0755 employee
projection. After mount preparation it verifies the exact file set and hashes,
read-only flags, mount ID, fixed MCP argument suffix and bridge ELF hash from the
actual descriptors delivered to B2. Only flat `RunConfig` destinations are
supported here; RuntimeHome/WorkingDirectory projections still refuse.
A root-owned 0640 `/run/duduclaw-spawnd/mcp-<runhex>.json` receipt binds those
observations to generation, employee and run. `McpRunReceipt::read` enforces boot,
run, mode and a 60-second consumption deadline; cleanup removes the receipt.
Fixture publications retain their explicit independent launch contract through
the default trait methods and are not proof of installed MCP acceptance.

### Installed publication root acceptance (not a Claude turn test)

The three `installed::root_acceptance::iso_root_e2e_installed_publication_*`
tests consume real installed F/B1 inputs. They create no acceptance metadata,
accounts, signed manifests or run receipts. Each requires root and
`DUDUCLAW_ISO_DISPOSABLE=1`; a missing daemon, signed artifact, confirmed employee,
fresh measurement or runtime acceptance causes a test failure. They verify only
the root publication and receipt-consumption layer. They do not prove
A→B1→systemd→B2 execution, a real Claude model turn, a D tool result, D8 queueing,
or fresh login attacks.

The VM must have the compiled-version F service running with a committed signed
generation; B1 must continuously publish current root/gateway reports, with at
least one confirmed MCP-enabled Claude entitlement. The signed runtime policy
must contain real exact-runtime MCP inheritance acceptance. The root measurement
harness must continuously renew the installation/SSH/DNS/kernel receipts. No test
synthesizes those prerequisites. For the third test, an externally started real
Claude run must remain journaled and its MCP receipt must be less than 60 seconds
old. Set `ISO_INSTALLED_RUN_ID` to that actual 32-character lowercase hex run ID.

Build on the Linux builder:

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --lib --no-run
```

Run each test separately on the disposable installed root VM (commands below are
intended for a root shell; they have not been executed by this development pass):

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::root_acceptance::iso_root_e2e_installed_publication_signed_policy_and_gateway_consumer -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::root_acceptance::iso_root_e2e_installed_publication_live_install_evidence -- --ignored --exact --test-threads=1 --nocapture
# Export ISO_INSTALLED_RUN_ID from the actual newly started run before this command.
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::root_acceptance::iso_root_e2e_installed_publication_actual_run_mcp_receipt -- --ignored --exact --test-threads=1 --nocapture
```

The acceptance harness can copy the emitted library test executable using its
existing `exes.txt` / `push.sh` convention. Its `runall.sh` filter
`iso_root_e2e_installed_publication_` selects exactly these three tests; the root
shell must also export the real `ISO_INSTALLED_RUN_ID` for the third test. Verify
that each result says `1 passed`, rather than treating zero selected tests as a
pass. The separate `iso_root_e2e_publication_receipt_mode_and_symlink_refusal`
fixture tests receipt filesystem rejection only and does not replace these tests.

### Real installed account denials (separate from Claude/D8 acceptance)

`installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials`
executes actual localhost SSH public-key login and remote-forwarding attempts,
`su`, a user-manager transient unit, crontab installation and `at` scheduling.
Every family must have a successful positive control. Missing binaries, missing
journals, unreachable services, timeouts, a failed UID transition or unrecognized
errors fail the test. It does not create accounts, keys, SSH policies, signed
runtime acceptance or installed authority records.

Use a disposable, already installed VM with an actual confirmed employee in the
root publication/UID registry. Keep its expired/locked password, `nologin`,
`/nonexistent`, no-linger and scheduler denial policies intact. The dedicated
control account must be an ordinary local login user (UID 1000–59999) with a
working user manager and permission to use cron, at and SSH remote forwarding.
Its crontab must contain exactly this single harmless line, including newline:
`# duduclaw isolation acceptance control`. The employee must have no crontab.
Both `cron.service` and `atd.service` must be active. `/usr/bin/setpriv` starts
scheduler/user-manager attempts under the exact UID/GID with supplementary groups
cleared, bypassing PAM so expiration cannot hide which tool was actually tested;
a UID marker proves execution. `su` separately tests the real PAM/login route.

The SSH fixture is deliberately explicit:

- The actual SSH daemon listens at `127.0.0.1:22`; this test never changes it.
- `/run/duduclaw-iso-account-test` and its `authorized_keys` directory are
  root-owned and traversable by sshd. Effective `AuthorizedKeysFile` must be
  exactly `/run/duduclaw-iso-account-test/authorized_keys/%u` for both users.
  Pre-provision one identical dedicated test public key in each root-owned
  `<username>` file. Thus absence of an employee key cannot explain a pass.
- Set `ISO_ACCOUNT_SSH_KEY` to that key's root-private, unencrypted test private
  key file, and `ISO_ACCOUNT_SSH_KNOWN_HOSTS` to a root-owned known-hosts file
  containing the independently verified localhost host key. No `ssh-keyscan`
  trust-on-first-use bypass or interactive credential prompt is used.
- Root must be able to read the current-boot sshd journal. Each negative attempt
  uses a fresh loopback source port through `/usr/bin/nc`; acceptance requires
  both the client authentication refusal and root-owned journal messages from
  the same sshd PID naming that employee/source port and explicitly rejecting it
  because of `DenyUsers`. Offline `sshd -T` plus exit 255 alone is insufficient.
  An sshd configuration/logging variant that cannot supply those precise records
  fails the test instead of being treated as a denial.

Required tools are `timeout`, OpenSSH client/server/keygen, `nc`, `journalctl`,
`su`, `setpriv`, `systemd-run`, `systemctl`, `crontab`, `at`, `atq` and `atrm` at
the fixed system paths in the test. Every command has a 12-second TERM deadline
and two-second KILL grace. SSH masters close even after unexpected success;
crontab uses only a comment-only table and removes an unexpectedly accepted
employee table; `at` schedules only `/usr/bin/true` without mail, verifies the new
job's owner and immediately removes that job before asserting the result.
The test does not remove existing employee/control jobs. If the active UID
monitor interrupts an attempt before its exact evidence is observed, the test
fails; it never disables that monitor or counts an arbitrary kill as success.

On the Linux builder:

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --lib --no-run
cargo test -p duduclaw-spawnd --lib installed::account_root_acceptance::ssh_policy_evidence_requires_same_root_session_and_source_port -- --exact
```

On the disposable installed VM, from a root shell after preparing the fixture:

```sh
export ISO_ACCOUNT_CONTROL_USER=iso-control
export ISO_ACCOUNT_SSH_KEY=/run/duduclaw-iso-account-test/client_key
export ISO_ACCOUNT_SSH_KNOWN_HOSTS=/run/duduclaw-iso-account-test/known_hosts
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials -- --ignored --exact --test-threads=1 --nocapture
```

For the existing copied-binary acceptance harness, export the same three fixture
variables in its root shell and use `runall.sh <outdir>
'iso_root_e2e_installed_account_real_denials'`. This command must select one test
and report `1 passed`; it is not evidence of a Claude turn, MCP tool call or D8
admission. This development pass type-checked the test but did not execute these
root attacks or provision the fixture.

## License

Apache-2.0, as declared by the workspace.
