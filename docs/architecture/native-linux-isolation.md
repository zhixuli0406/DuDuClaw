# Native Linux isolation implementation notes

This is implementation status, not a claim of enabled isolation or acceptance.
The shared v1 contract is in `crates/duduclaw-isolation`.

## Package B1

Status: **Linux host wiring implemented; default composition remains fail closed.**
`main` now selects `host::LinuxHost`, not the unavailable test host. Its installed
socket, account registry, recovery, systemd operations, relays, cancellation and
readiness publication are real Linux code. The default `Components` intentionally
supplies missing B2/H/F/G evidence adapters and missing C DNS evidence, so it cannot
admit employee runs. No gateway execution path has been changed by B1. Compilation
is not native Linux acceptance or independent security approval.

### This round: disposition of the previous seven items

| Previous item | B1 implementation now | Integration / acceptance still required |
| --- | --- | --- |
| Production Host | `LinuxHost` composes registry/accounts, systemd, journal, scanner, output relays, endpoint revokers, B2/C/H interfaces and root publication. Slow lifecycle work uses a bounded mailbox; control requests do not wait for it. | F/G must construct `Publication` and installation evidence; B2/H must implement their adapters. |
| Bootstrap and mount delivery | Fixed `/usr/lib/duduclaw/duduclaw-run`, argv containing only that path, fd 0 root seqpacket and fd 1/2 bounded pipes. B1 builds/seals the plan and prepares detached private mounts, minimal `/etc`, root projections and digest-checked executable/script fds. | B2 must explicitly accept this internal bootstrap version and implement nonce exchange, exact rights delivery, cancellation barrier and all isolation/PTY readbacks. |
| Restart reconciliation | Permanent allocation tombstones; orphan unit inventory/stop/reset-failed; no-UID scan; C unregister; H persistent attachment inventory/detach; projection cleanup; durable run-journal removal only after proof. Old in-memory gateway reservations cannot survive a new daemon. | C/H inventories and acknowledgements must be available; unprovable cleanup leaves readiness false. |
| Publication and provisioning | Typed root-only `Publication`; `RegistryStore::allocate/reconcile`; account creation/quarantine/readback; H provision/destroy request binds operation id and full proposal digest. | Signature verification/publication belongs to F; approved proposal authority belongs to G. Normal provisioning runs before activation or while the registry owner is stopped, not through gateway StartRun. |
| Readiness evidence | Atomic root-owned, gateway-readable evidence; boot/time expiry; registry/account/SSH/IAM, B2, C/DNS, H, resource and scanner checks. `--check` returns the same evidence plus failure status. | Actual SSH login/forwarding negatives, IAM reservation/owner scan, B2 self-tests and C DNS route proof cannot be replaced with booleans in config. |
| Live I/O and lifecycle | Revocable MCP/egress pairs; acknowledged C registration; stdin/output pipes; bounded relay pumping and truncation audit; PTY ownership retained before terminal cleanup; cancelled preparation fenced before B2 release. | B2 supplies actual validated per-run PTY and status. D must not activate a revoked terminal-run MCP endpoint. A must retry bounded `NotReady` TakeIo/Resize responses. |
| Installer and acceptance | Installer stages root registry/log/projection directories, fixed binaries/units and locked supervisor; service permits account quarantine; ignored root tests added. | F/G publish the signed compatible generation and activate it; A/F/C join the TCB slice; three-distro root execution and Daybreak review remain required. |

### Authority, ownership and fail-closed lifecycle

The shared gateway IPC v1, canonical signing bytes, UID rules and hard limits were
not changed. Runtime/profile/argv/environment/limits remain root-derived; the
caller cannot provide a path, UID, unit property, mount source or policy override.
`RootPolicy` rejects unknown or ambiguous profiles. A missing component never
selects a gateway-UID fallback.

The listener is `/run/duduclaw-spawnd/control.sock`, root-owned `0660` with the
installed gateway group, authenticated by exact peer UID/GID. Activation validates
the actual descriptor, bound path and listening state. Standalone mode creates
and verifies the fixed root parent. Under the registry singleton lock, it only
removes an existing trusted socket after `ECONNREFUSED` and inode/device readback;
a live socket, wrong owner/mode or unknown error is refused.

`RegistryStore` holds the root allocation flock for its lifetime. State lives in
`/var/lib/duduclaw-spawnd` (`0700`); registry and active-run journal are root-only
`0600`, temp-write + file fsync + rename + directory fsync. Failed durability
poisons the writer instead of allowing a later successful write to hide the
failure. Employee UID mappings are permanent and never recycled. Registry
initialization rejects writable ancestors, symlinks, wrong ownership and hardlinks.

An interrupted `Reserved` allocation is first durably tombstoned. Recovery checks
exact NSS/local/userdb identities before quarantining a group-only or half-created
account. It locks/expires the password/account, installs nologin and `/nonexistent`,
removes supplementary groups, disables linger and user managers, fixes effective
cron/at ACLs, reloads SSH, and reads the results back. Unknown aliases or jobs are
not silently accepted. User creation disables subordinate-ID allocation and
lastlog initialization. External IAM range reservation, complete existing-owner
scan and live SSH public-key/forwarding negatives remain explicit evidence hooks;
`MissingInstallationEvidence` always refuses them. `userdbctl` supports bounded
JSON arrays or object streams and rejects unknown record shapes.

The service now allows `/etc` and the existing linger directory to be written for
root-only crash quarantine; normal provisioning remains the installer/G operation.
This is an explicit change from the first-round read-only service policy. The
service retains the root capabilities required by its account tools and mount
preparation. No gateway RPC for arbitrary account mutation was added. Installation
must provide explicit login.defs UID/GID range entries; missing compiled-default
range evidence remains a refusal, not a guessed distro default.

`runs.json` is written before H attachment, capability delivery or unit creation.
A lost StartTransientUnit or H attach reply is recovered by inventory, not treated
as no effect. Cleanup closes B2 originals, recovers/detaches every matching H
attachment, removes its fixed projection, then repeats unit/cgroup/UID checks.
It keeps the journal and D8 lease on any uncertainty. Ended output endpoints remain
until one-shot TakeIo or retention expiry; capability endpoints are already revoked.

The monitor runs separately from lifecycle/account/D-Bus work. It rechecks the
current admission generation after scanning `/proc`, sets a shared cancellation
fence on intrusion/failure/deadline, revokes sockets, and signals employee processes
through pidfds with UID/start-time/cgroup revalidation. Unit stops use an independent
bounded emergency queue. No D-Bus call holds the monitor map lock. Readiness rejects
stale/error scan results, and account failures do not turn an unreadable scan into
an empty result. This is a fail-closed deadline mechanism, not a measured two-second
latency guarantee under kernel stalls or very large process tables.

The lifecycle mailbox holds at most 128 commands/results, with bounded cached
slots and finite operation deadlines. Worker timeout does not make an OS operation
transactional: cancellation fences prevent a delayed preparation from releasing
B2, and journal/inventory recovery preserves partial effects. Relay errors poison
readiness and cancel the affected run. SIGTERM/SIGINT cancel and tick cleanup for
up to ten seconds. Transient units bind to `duduclaw-spawnd.service`, so PID 1 stops
them if the daemon exits or is killed; crash residue still requires reconciliation.

### systemd and B2 launch contract

`Systemd::start` calls D-Bus `StartTransientUnit`, with `AddRef` in the creation
transaction, fixed trampoline ExecStart, real Unix fd variants and all root-selected
hardening/resource properties. It verifies configured values and actual cgroup
memory/swap/PID/CPU/IO ceilings. Cleanup handles failed, inactive, missing and
half-started units without interpreting an unknown state as proof. Unit references
are released only after cleanup. `resources_ready` checks TCB/parent memory
reservations, aggregate employee ceilings, trusted service slice placement and
host PTY capacity (`max - reserve`, with 10% headroom).

B1's internal `BootstrapPlan` version 1 is an **integration proposal for B2**, not a
change to shared gateway protocol v1 and not a claim of compatibility with the
parallel B2 implementation. It binds domain/version, nonce, run/employee, static
employee and supervisor identities, runtime version/profile, fixed argv and fresh
environment, quotas and an exact ordered fd manifest. Plan bytes use a bounded
memfd with WRITE/GROW/SHRINK/SEAL seals. The dedicated bootstrap transport must
support up to 80 rights; it must not reuse the gateway transport's 65-right limit.

The manifest contains the sealed plan; ELF/interpreter and optional script;
home/work/minimal-etc/runtime/employee-projection detached mounts; prompt; optional
stdin; ordered attachments; optional MCP and egress. Runtime destinations remain
stdin 0, MCP 3, prompt 4 and attachments 5 onward; executable/script descriptors
are handled by B2's high-fd algorithm. Runtime source count and NOFILE requirements
remain those of the approved design. No secrets or employee payload enter argv,
environment, audit text or journal records.

`bootstrap_mounts::prepare` clones H home/work mounts before changing attributes,
sets recursive private propagation, and checks restrictive flags/mount identities.
Work is conservatively noexec. It creates the five fixed minimal-etc files under
a root-only anchor and fsyncs publication. Employee projections come from the
fixed EmployeeId hex child of a root-published projection anchor, with recursive
RO/NOSUID/NODEV/NOEXEC/NOSYMFOLLOW. Runtime executable/script openat2 is relative to
the detached runtime mount, forbids links/traversal/cross-mount resolution, checks
root ownership/ELF/statx identity and SHA-256, and rechecks after hashing. Hashing
has a 512 MiB per-file cap and a shared 15-second loop deadline; kernel-stalled I/O
is fenced externally, not forcibly undone by a thread timeout.

`Trampoline::prepare` receives B1's already sealed plan and owned prepared fds;
it must keep the release barrier closed. `bind_unit` receives the actual systemd
receipt and shared cancellation flag. It must check that flag immediately before
release. `poll` may report Running only after complete namespace, capability,
UID/GID, seccomp, Landlock and fd readbacks. `take_pty` transfers the validated
per-run devpts master once; B1 retains it through short-command completion.
`close` is idempotent even after a partially failed prepare and must drop originals.
The shipped `UnavailableTrampoline` refuses every operation.

### H and publication seams

`storage::Storage` is the B1-facing adapter for `duduclaw-isolation-storage`:
`provision`, `attach`, `detach`, `destroy`, `usage`, `readiness`, `attachments`.
No image path, raw image fd or loop device goes to B2. Attachment receipts bind
employee/run and H's durable generation token. Usage must prove hard byte/inode
limits, backing-device graph and host reserve. Attach failures can have effects;
H's persistent inventory must expose them. Detach must be generation-safe and
idempotent. Provision/destroy bind G's operation ID and full approved digest;
neither destroys nor recycles a UID mapping. No H implementation was duplicated.

`Publication` supplies F/G's verified root policy, gateway snapshot mount authority,
supervisor identity and root runtime/projection anchors. It is not deserializable
from StartRun. `InstallationEvidence` supplies actual IAM/owner-scan/SSH results.
C uses the authenticated fixed control endpoint, correlated Register/Unregister
acknowledgements and a separate DNS/secondary-layer evidence adapter. Missing
adapters and unknown evidence reject readiness. C's installed socket ownership and
peer credentials still need compatibility acceptance with its parallel package.

### Gateway readiness evidence

A reads `/run/duduclaw-spawnd/readiness.json` or invokes `duduclaw-spawnd --check`.
The file is atomic root:gateway-group `0640`; readers validate root ancestry,
regular type, no links, exact ownership/mode, bounded JSON, schema version, boot
ID and monotonic boottime expiry. The check command works as the gateway account,
prints the structured report, and exits nonzero when unavailable/stale.

The report has no secrets, command lines, host process IDs or free-form errors.
It contains the primary `Readiness` booleans, secondary BPF status, observed host
PTY budget and timestamps. Evidence is dated at probe **start**, with a maximum
one-second report lifetime; scanner evidence must be less than one second old at
publication. Expired/missing evidence denies. Slow probes can therefore keep a
large installation unavailable; no performance acceptance has been claimed.
Production ReserveRun/policy selection uses the same verified in-process view,
not an extra per-request slow-probe RPC. Start performs fresh host/UID/storage
checks before effects. A must not equate service active or a file's existence with
readiness, nor treat a failed check as permission to use the legacy executor.

### Validation and remote handoff

This round ran only the small contract/spawnd crates in this worktree. No commit,
worktree creation/removal, gateway/CLI/workspace build, cargo fmt, deployment,
real account mutation or remote root test was performed. This session did not
create this worktree; it remains intact with the requested uncommitted handoff.

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation -p duduclaw-spawnd --offline` | 29 contract + 45 spawnd tests passed on macOS; 0 failed. Includes five mailbox regression tests. |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --tests --target x86_64-unknown-linux-gnu --offline` | Passed; Linux-only/root test bodies compiled, not executed. |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --tests --target x86_64-pc-windows-gnu --offline` | Passed; Windows test bodies compiled, not executed. |
| `git diff --check` | Passed for tracked changes; new Rust/text files also checked for trailing whitespace. |

The first check found an invalid workspace-inherited sha2 dependency; the existing
locked `sha2 0.10.9` is now a direct Linux dependency. Linux compilation then found
a duplicate Clone derive and one ambiguous closure Result type; both were fixed.
Unsafe-block lint notices and the pre-existing fetch_update deprecation remain.
Independent code review found and drove fixes for cgroup membership formatting,
partial-start cleanup, H lost receipts, scan generations/cancellation, evidence
freshness, durability poisoning, short-lived PTY retention and shutdown draining.
This is **not** the required `gpt-daybreak-blue-latest` security review; that review
and live acceptance remain outstanding.

On each disposable Ubuntu 22.04/systemd 249, Debian 12/systemd 252 and Ubuntu
24.04/systemd 255 VM, run as root with the compatible package fixtures installed.
The account test changes login ACLs and uses fixture UID 99997; do not run these
on a production host or concurrently with a live spawnd. Resource-readback tests
need the complete installed TCB units; missing components should fail that test.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation -p duduclaw-spawnd --offline
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1
cargo test -p duduclaw-cli spawnd_install_tests
```

Individual root filters (crate `duduclaw-spawnd`):

- `iso_root_fixed_socket_bind`
- `iso_root_intruder_is_signalled_through_pidfd`
- `iso_root_registry_restart_quarantines_reserved_uid_and_preserves_tombstone`
- `iso_root_registry_rejects_untrusted_state_directory`
- `iso_root_half_created_account_is_locked_and_read_back`
- `iso_root_systemd_fd_delivery_and_restart_reconciliation`
- `iso_root_systemd_resource_hierarchy_readback`
- `iso_root_bootstrap_plan_seals_and_readback`
- `iso_root_mounts_private_recursive_readonly_and_exec_identity`
- `iso_root_mounts_projection_residue_cleanup`
- `iso_root_run_journal_fsync_failure_poison_and_reopen`
- `iso_root_readiness_report_atomic_mode_and_boot_validation`

Focused non-root filters: `admission::tests`, `registry::tests`, `accounts::tests`,
`policy::tests`, `systemd::tests`, `readiness::tests`, `bootstrap::tests`,
`storage::tests`, `host_worker::tests`, `host::tests`, `host_state::tests`,
`fds::tests`, `transport::tests`, `channels::tests`, `server::tests`, `audit::tests`.
The systemd ignored FD test uses test-only fixed cat/sleep/false fixtures; it proves
PID 1 fd delivery/recovery, not a real B2 isolated employee run.

### Changed file inventory

| Files | Responsibility |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `crates/duduclaw-spawnd/Cargo.toml` | New workspace member; reuse locked dependencies, including Linux SHA-256. |
| `src/host.rs`, `src/host_worker.rs`, `src/host_worker_tests.rs`, `src/host_state.rs` | Production composition, separate scanner/cancellation, bounded lifecycle jobs, durable runs and expiring readiness. |
| `src/bootstrap.rs`, `src/bootstrap_mounts.rs`, `src/storage.rs` | Sealed B2 contract, concrete root fd/mount preparation, H adapter API. |
| `src/accounts.rs`, `src/registry.rs` | Permanent identities, account lockdown and restart quarantine. |
| `src/systemd.rs` | Fixed transient launch, AddRef, actual resource readback and orphan recovery. |
| `src/main.rs`, `src/server.rs`, `src/transport.rs`, `src/fds.rs`, `src/lib.rs` | Actual host selection, check/serve, shutdown drain, authenticated sockets and descriptor validation. |
| `src/admission.rs`, `src/admission_tests.rs`, `src/policy.rs` | D8/queue/timeout state machine, async polling/shutdown tests, root policy. |
| `src/channels.rs`, `src/egress.rs`, `src/audit.rs` | Bounded output, capability revocation, control acknowledgements and typed audit. |
| `crates/duduclaw-cli/src/service/protected_linux_install.rs` | Optional signed-binary-dependent staging and template tests; not compiled locally. |
| `scripts/linux-protected/spawnd/*` | Socket/service/resource unit templates and installation boundary. |
| crate README, this note, `docs/README.md`, `CHANGELOG.md` | Current development status, compatibility seams and exact acceptance filters. |

No shared contract change and no new dependency version. The three public security
feature pages remain unchanged because employee isolation is not enabled. B1 uses
[systemd v249's fd setter](https://raw.githubusercontent.com/systemd/systemd/v249/src/core/dbus-service.c)
and [execution properties](https://raw.githubusercontent.com/systemd/systemd/v249/src/core/dbus-execute.c);
source inspection does not substitute for the native tests above.

The fixed account utility paths follow the [Debian passwd file list](https://packages.debian.org/bookworm/amd64/passwd/filelist)
(`/usr/bin/chage`); scheduler inspection uses [at -l](https://manpages.debian.org/bookworm/at/at.1.en.html)
instead of executing the atq symlink. These paths still require distro acceptance.
