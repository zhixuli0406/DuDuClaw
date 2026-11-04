# Native Linux isolation implementation

This is an implementation handoff, not evidence that protected employee isolation
or a production update is ready. Each implementation package owns its section.

## Package F

Status: **Round 2 implementation present; integration and native acceptance pending.**
The production Linux activator now switches `current`, stops/restarts fixed units,
checks genuine live readbacks, and advances the floor only after health. It uses
real socket/systemd adapters, not `UnavailableActivator`. Missing A/B/C/G seam
endpoints fail closed before publication. No customer service was enabled, no
commit was made, and no gateway/CLI/workspace build ran in this worktree.
Provisioning and cross-package wiring remain integration work; this source must
not replace an existing v1.79.3 installation by itself.

### Interfaces and trust

`duduclaw-updated` is a workspace library and executable. All OS adapters are
behind `cfg(target_os = "linux")`; the portable verifier builds on macOS and
Windows. No `duduclaw-isolation` schema, canonical capability bytes or hard limit
was changed. Update frames are the existing `UpdateRequest`/`UpdateResponse`
enums, encoded as JSON behind the shared four-byte length prefix, one seqpacket
per request. The endpoint is the contract's fixed `UPDATED_SOCKET`.

The server authenticates numeric UID/GID with `SO_PEERCRED`, accepts only the
registered gateway account or root (operator/nightly requester), and accepts no
ancillary data. Zero ancillary receive space causes Linux to close discarded
SCM_RIGHTS descriptors; `MSG_CTRUNC` and `MSG_TRUNC` reject the packet. Requests
have a five-second receive/send timeout and a 16-connection backlog. A single
worker performs updates while the server answers status. The latest 32 request
records are durable; an evicted or unknown ID is an unknown outcome, never proof
of failed installation. Apply requires an exact stable version; the gateway
chooses it from its recent update check and never forwards an asset URL.

The dashboard Apply and Status dispatch entries require Admin. The UI preserves
its request ID when the server response is lost, polls across gateway restarts,
and reports success only for `committed`. The CLI now requires `--version` and
submits the same RPC, retaining the existing AI-session operator guard. The
nightly timer retains its schedule but invokes the frozen launcher with
`--check-latest`. The unsigned GitHub index only selects a candidate: strict JSON,
`draft=false`, `prerelease=false`, stable tag, one MiB response ceiling. Missing
service/trust/state refuses; there is no legacy privileged install fallback.

Metadata encoding is compact UTF-8 JSON with lexicographically sorted object
keys, integer numbers, original array ordering, and no trailing newline.
`decode_canonical` round-trips the typed value to reject duplicate/unknown fields,
whitespace, alternate number/escape encodings, and ambiguity before signature use.
The metadata limit is 256 KiB. This is package F's signed-file representation;
it does not alter the shared wire contract.

The SSHSIG verifier follows upstream [PROTOCOL.sshsig](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.sshsig)
and [PROTOCOL.u2f](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.u2f).
It requires version 1, SHA-512, empty reserved bytes, fixed role namespace, exact
public blob and algorithm, canonical base64, no trailing envelope bytes, and
strict Ed25519 verification. SK Ed25519 requires the role-specific application
and UP+UV flags; unsupported extension flags fail closed. The authenticated
counter is returned for diagnostics, not used as a monotonic authorization.
Root trust requires three distinct registered keys, at least one SK key, and two
distinct signatures. Rotation checks both old and new thresholds, adjacent root
version, expiry and permanent revocations. The target key is separate and must
be hardware-shaped. Synthetic SK vectors are parser/crypto evidence only, not
hardware evidence. SK ECDSA is unsupported and rejected.

The release signer is `scripts/sign-isolation-manifest.py`. It takes a manifest
template plus raw files named by artifact ID, recomputes size/SHA-256 through an
opened regular-file fd, and asks `ssh-keygen -Y sign` to use the fixed namespace.
Keys stay external. Outputs are `manifest.json` and
`manifest.json.signatures.json` (exactly one armor string for a release; a root
may have multiple). Publish them as `isolated-linux-<arch>-manifest.json` and
`isolated-linux-<arch>-manifest.json.signatures.json`; artifact assets are named
`isolated-linux-<arch>-<artifact-id>`, under the official `vX.Y.Z` release. The
plain Ed25519 fixtures were generated with system OpenSSH 10.5p1; no private key
is retained. Real FIDO2 fixture instructions are in the fixture README.

### Storage and process boundaries

All root anchors and their ancestors must be root-owned, without group/other
write access. Child operations use directory fds and `openat2` with BENEATH,
NO_SYMLINKS, NO_MAGICLINKS and NO_XDEV. A regular file must have one link, root
ownership, and no group/other write bits. Atomic snapshots use a new file,
file fsync, rename, then directory fsync. A root-owned flock serializes processes.
A persistence failure poisons the writer permanently, including after rename
may have landed. Subsequent status/error handling cannot save an older snapshot;
only a new process may read back the disk and recover. A regression test injects
a post-rename error and confirms the landed floor is never overwritten.

The downloader is `/usr/bin/curl`, executed only after dropping all groups and
all real/effective/saved IDs to `duduclaw-dl`, clearing the environment, setting
non-dumpable and NoNewPrivileges. It receives a fixed official HTTPS URL and no
credentials. Redirects stay HTTPS and are capped at three; transfers are bounded
by 305 seconds. Bytes stream through a pipe into a root-created staging inode;
the downloader never owns or opens an installation file. Hash and size checks
occur during copying and again on the resulting root-owned fd. Per-artifact
limit is 2 GiB; a generation totals at most 8 GiB. Downloaded archives are not
extracted. Artifact destinations come solely from `InstallTarget`.

The current accepted generation requires gateway, spawnd, trampoline, egress,
forwarder, MCP bridge, parser, importer and updater, all with protocol 1. Wrong
product/architecture/minimum version, incomplete generations, unknown protocols,
expired/future manifests and non-increasing versions/serials are rejected.
Runtime-entry publication and forward data migrations use the typed
`GenerationPreparation` seam; a missing/refusing endpoint blocks staging.

Root layout to be provisioned by G after bootstrap authentication:

| Location | Content |
| --- | --- |
| `/usr/libexec/duduclaw-updated-launch` | Frozen launcher binary; only bootstrap replaces it |
| `/etc/duduclaw-updated/trust.json` | `TrustBundle`: canonical root string, signatures, independently pinned digest |
| `/etc/duduclaw-updated/slot-{a,b}.json` | Accepted signed manifest, signature and acceptance timestamp |
| `/var/lib/duduclaw-updated/state.json` | `State`: floor, serial, digest, high-watermark, pending journal, selector, bounded request records |
| `/var/lib/duduclaw-updated/trust-ledger.json` | Signed root history, accepted releases, emergency intents and outcomes |
| `/opt/duduclaw-isolated/current` | Atomic relative symlink to one verified generation |
| `/var/lib/duduclaw-updated/update.lock` | Lifetime flock |
| `/opt/duduclaw-isolated/generations/<sha256>` | Immutable complete verified generation |
| `/opt/duduclaw-isolated/slots/{a,b}/duduclaw-updated` | Root updater A/B executable inodes |
| `/opt/duduclaw-isolated/pending-generation` | Bounded flat scratch stage, safely removed and recreated after interruption |

Provisioning must supply root-owned directories and initial state, gateway and
downloader system accounts, and the systemd unit. The executable refuses to
invent an initial floor or trust root. `verify_bootstrap_root` compares a compiled
`DUDUCLAW_BOOTSTRAP_ROOT_SHA256` against canonical bytes and verifies two root
signatures. An absent compiled pin refuses. G must still implement the independent
website/console hash comparison and both migration-package signatures before
provisioning; this function alone is not bootstrap acceptance. Never use curl
piped to a shell. No production trust material is included in this package.

### Durability and A/B recovery

`State` stores the committed version/serial/digest together in one atomic
snapshot, avoiding a split commit across a selector and a version-floor file.
A separate serial high-watermark survives abandoned trial boots, so a failed
serial cannot later authorize different bytes. Only the same pending digest can
retry its serial. Tests enumerate version, serial, expiry, architecture, product,
protocol and complete-generation failures.

`stage_inactive_slot` copies the verified generation's updater into a new inode
in the inactive slot, fsyncs the file, slot and parent directories, writes its
receipt, re-verifies it, then persists the trial selector. Repeating a pending or
already healthy trial returns that same slot; it never overwrites the known-good
fallback. The launcher persists `attempts=1` before `execveat` on the very fd it
hashed. A failed first root run causes the next launch to select the verified
previous slot, leave the committed floor unchanged, and retire an unactivated
pending attempt. A higher fixed release may then be accepted. Missing valid
active/fallback slots durably enter maintenance; the launcher does not guess a
stale other slot. Startup verifies its executable inode against the selected
signed slot, trust and journal access, accounts, and socket bind/listen before
marking a trial healthy. Slot receipt freshness is evaluated at acceptance time;
current root validity and revocations still apply. An update freeze does not
revoke an already accepted slot; new release admission still refuses it.

`GenerationActivator::activate_and_read_back` is the explicit A/B/G seam. It
must drain employee runs, publish a single current-generation selector, restart
fixed units, and compare genuine non-root product/version/build/protocol and
service-readiness results against the signed generation. It must coordinate
`stage_inactive_slot`, service restart and continuation after updater self-update.
Only a matching healthy updater slot can advance the journal to activated and
commit. `LinuxGenerationActivator` is now the daemon default; the unavailable
implementation remains only a refusal test fixture.

### Round 2 transaction and recovery

The durable order is request admission → download and hash → immutable generation
rename/fsync → trust acceptance receipt → `Prepared` → inactive updater slot
copy/receipt/fsync → trial selector → process exit 75 → systemd restart through
the frozen launcher → durable attempt → root self-test → healthy slot → drain
employee admission → stop the gateway and verify MainPID is zero → G migration /
B runtime publication → single `current` symlink rename/fsync → fixed unit
restarts → live health and executable readback → `Activated` → atomic floor,
serial, digest and `resume_required` commit → emergency-history completion if
needed → fresh health and admission resume → final public `Committed`.

`State::status` and idempotent Apply replay hide an internal committed record
while resume is outstanding. Failed/lost Resume stays visibly activated or
maintenance; a restart revalidates health and retries the same digest. The floor
is not reverted after a landed commit. Each clean daemon boot closes admission
and verifies the actual running generation, even when no pending update exists.
Unexpected/missing `current`, unreadable artifacts or mixed live versions keep
admission closed. `current` alone is not readiness evidence.

Queued/downloading requests resume automatically. A partial root scratch stage
is unlinked through its trusted dirfd and recreated; deletion is bounded and
rejects links, mounts, directories and special files. A crash after publishing
the immutable directory but before preparing state reuses and rehashes it.
Prepared/activated requests resume with their original digest, serial and
acceptance time. An elapsed release expiry does not invalidate a durably
accepted transaction; current root expiry and revocations still apply. A failed
first root trial retires its pending request, preserves the previous generation,
and does not lower the serial high-watermark. Unknown persistence outcomes poison
the writer so stale in-memory state cannot overwrite a possibly landed rename.

Before downloading, bounded GC keeps the effective and committed generations,
the candidate, and both slot-receipt generations. All other flat generation
directories are removed safely; interrupted GC is reentrant. Allocation checks
require the artifact total plus one maximum updater slot and 512 MiB free, and
at least 256 available inodes. Existing generations are never overwritten.

`Directory` root tests inject failures after temporary cleanup/create/write,
file fsync, rename, destination fsync and source fsync, then reopen the actual
snapshot. Portable tests inject failure after every production activation effect
and every normal/emergency durable state transition. These simulate process
interruptions; they are not physical power-loss evidence.

### Cross-package integration contract

The existing isolation update RPC is unchanged. The following internal F-owned
seam types are in `activation_contract.rs`; they are not exposed to employees or
the dashboard. Their exact wire representation is serde JSON in the existing
bounded four-byte-length seqpacket frame, with no ancillary descriptors.
Every request/response binds schema version 1, a fresh random nonce and an exact
manifest digest. Unknown fields, extra/missing components and refusal responses
fail closed. Adapters enforce bounded I/O/retry deadlines.

| Owner | Seam / obligation |
| --- | --- |
| B1 | `ActivationRuntime` on `/run/duduclaw-spawnd/update.sock`, root peer only. `Drain` durably closes admission and proves zero active runs. The barrier survives spawnd restart and is keyed by digest. `Resume` is idempotent after a lost reply; never implicitly resume on disconnect/restart. |
| B1/G | `GenerationPreparation::{preflight,apply}`: typed digest-bound migrations and runtime artifacts. Preflight makes no data changes. Apply is idempotent and recovers any interrupted publication/migration; updater first drains and stops the gateway. Missing reverse-migration support causes emergency rollback across **any** migration to be refused. |
| A | Live gateway readback at `/run/duduclaw-gateway-update/readback.sock`, peer UID/GID from the provisioned `duduclaw` account; exactly one gateway component. |
| C | Live egress readback at `/run/duduclaw-egress-update/readback.sock`, peer UID/GID from `duduclaw-egress`; exactly one egress component. |
| B1/B2/D/E/G | B1 supplies actual isolated readiness results for non-daemon artifacts. It cannot stand in for gateway/egress endpoints. Report product, version, protocol, actual loaded executable SHA-256, effective UID and readiness for every signed artifact except the updater slot, which has its independent root self-test. Root execution is allowed only for spawnd/trampoline. |
| G | Provision independent bootstrap pin/dual migration signatures, accounts, initial state/receipts, frozen launcher, slots, units, `current`, and trusted anchors. Keep install/generations traversable for service UIDs. Non-root readback daemons bind their own socket in a dedicated service-owned leaf under root-owned `/run`; verify root client credentials. |

`LinuxGenerationActivator` checks `current` is the committed/effective or pending
digest, verifies every artifact's bytes and signed mode, publishes the directory
0755 and `current` atomically, then restarts only `duduclaw-egress.service`,
`duduclaw-spawnd.service`, `duduclaw-gateway.service`. Before restarting, it checks
unit User/Group and non-root NoNewPrivileges settings. The live services must be
active, keep the same MainPID across authenticated readback, and `/proc/PID/exe`
must hash to the signed artifact both before and after that readback. The PID is
an observation, never an authorization principal. The manifest has no separate
build-string field; `build_sha256` is the exact signed executable identity.

The updater unit needs `CAP_SYS_PTRACE` to read non-dumpable service executables
and `CAP_DAC_OVERRIDE` to reach private service-owned health sockets, in addition
to its existing uid/gid/chown capabilities. It has ordering after the three fixed
units without a Requires dependency that would stop the updater during their
restart. Root VM tests cover the restricted capability set, a non-dumpable
non-root target and a mode-0600 seqpacket socket.

### Root trust administration and emergency rollback

`trust-ledger.json` preserves the bootstrap pin, adjacent dual-threshold root
rotations and accepted historical target identities. A root rotation also
persists freeze and permanent revocation. Historical installed generations remain
verifiable after an old root expires, but current root expiry/revocation still
refuses them. Histories are bounded: 32 roots, 256 accepted releases, 32 emergency
outcomes; exhaustion refuses rather than dropping security history. Atomic file
size is additionally limited to 1 MiB. No production signing keys are present.

A root operator stops the updater before using these fixed-inbox commands:

```sh
# CONFIG_ROOT/root-update.json: {"document":"<canonical signed root JSON>","signatures":["<armor>","<armor>"]}
/usr/libexec/duduclaw-updated-launch --rotate-root
# CONFIG_ROOT/emergency.json: same envelope, root-threshold emergency namespace
/usr/libexec/duduclaw-updated-launch --emergency
# Preparation only; start the service to continue the verified A/B transaction.
systemctl start duduclaw-updated.service
```

The root commands share the daemon's nonblocking update lock and cannot run
concurrently with an update. `--emergency` authorizes only an already accepted,
exact target digest, same product/architecture and an older version. It consumes
the emergency serial in trust state **before** writing the pending update state;
startup reconciles either snapshot boundary. The signed authorization and its
acceptance time are preserved. Expiry limits initial admission; the identical
accepted transaction can resume after that time, without accepting a new digest
or authorization. Current root checks remain live. A failed trial is recorded as
a consumed failed emergency, never replayed. A successful emergency changes the
effective generation and slot, while retaining the ordinary high version floor,
serial and highest attempted serial. Subsequent ordinary updates must exceed the
high floor **and** satisfy minimum-version against the actual running version.

### Remaining external gates and explicit refusals

- A/B/C/D/E/G must wire their named seams and G must provision the independently
  authenticated migration/bootstrap package. F neither invents readiness nor
  bypasses a missing endpoint. The shared isolation schemas were not changed.
- Pro's isolation asset publication URL/naming/authentication contract is not
  supplied by this checkout; its downloader route remains explicitly refused.
  The ordinary Pro/minisign pipeline is not a substitute for an isolation feed.
- Emergency rollback across any migration remains refused until G supplies a
  verified reverse plan. Forward migration/runtime publication uses the complete
  typed seam above; missing implementations refuse.
- Generate real hardware SK fixtures and verify GoTrust EdDSA. SK ECDSA remains
  unsupported/fail closed if the hardware requires it. Synthetic SK tests do not
  establish hardware compatibility.
- Run the ignored native Linux/systemd tests on Ubuntu 22.04/24.04 and Debian 12,
  then the integrated full update, reboot/power-cut, bootstrap and dashboard
  roundtrip. macOS tests and Linux cross-checks do not establish those outcomes.
- Remote gateway/CLI builds, web checks with dependencies installed, and the
  independent `gpt-daybreak-blue-latest` review remain required. Ordinary code
  review was performed; no Daybreak model was callable in this session.

### Validation commands

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --tests --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-updated --offline
python3 -m unittest discover -s scripts/tests -p 'test_sign_isolation_manifest.py' -v
python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v
# Root VM, systemd as PID 1, python3 and provisioned duduclaw-dl account:
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated -- --ignored iso_root_ --test-threads=1
# Assigned remote builder only:
cargo test -p duduclaw-gateway protected_update_
cargo test -p duduclaw-gateway restart_pending_tests
cargo test -p duduclaw-cli service_protected_
# Real FIDO2 fixtures must first be created per tests/fixtures/README.md:
cargo test -p duduclaw-updated real_fido2_sk_ed25519_openssh_vector -- --ignored
# Only with web/node_modules present:
cd web
npx tsc -b
npx vitest run src/lib/protected-update.test.ts src/components/settings/sections/UpdateTab.test.tsx
```

### Round 2 local results and file inventory

| Check actually run | Result |
| --- | --- |
| `cargo test -p duduclaw-updated -p duduclaw-isolation --offline` with jobs 4, incremental 0 | 63 passed: 34 updater + 29 unchanged contracts; 0 failed; 1 ignored real-FIDO2 test |
| `cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --tests --offline` with jobs 4, incremental 0 | Passed, including compilation of 11 ignored Linux root tests; 30 expected unsafe-boundary warnings in test build |
| `cargo check --target x86_64-pc-windows-gnu -p duduclaw-updated --offline` with jobs 4, incremental 0 | Passed; no native Windows execution |
| Python signer suite | 4 passed, including actual OpenSSH sign/verify |
| Python nightly helper suite | 12 passed, 1 skipped because systemd-analyze is absent on macOS |
| `git diff --check` | Passed |
| Web dependency check | `web/node_modules` absent; tsc/vitest not run |
| Native Linux / systemd / real FIDO2 / physical power cut | Not run on this macOS host |
| Independent ordinary code review | Identified startup/readback capability, cold-boot race, premature public success and emergency recovery issues; corrected and re-reviewed without remaining identified high/critical correctness blockers. This is not Daybreak acceptance. |

No external dependency, shared contract, gateway/CLI/web source or Cargo manifest
was changed in round 2. Their round 1 edits remain uncommitted. No `cargo fmt`,
commit, release, deployment or customer-machine operation was performed. This
session did not create a worktree, so the assigned worktree is retained for the
integrator.

| Round 2 file | Change |
| --- | --- |
| `crates/duduclaw-updated/src/activation_contract.rs` | New typed drain/preparation/readback seams, live identity validation, production orchestration crash matrix |
| `crates/duduclaw-updated/src/linux/activation.rs` | New real current publication, fixed-unit restart, authenticated health and executable readbacks, 3 root tests |
| `crates/duduclaw-updated/src/trust_store.rs` | New atomic trust history, rotation/freeze/revocation, emergency authorization and recovery tests |
| `crates/duduclaw-updated/src/linux/trust_admin.rs` | New root-only fixed-inbox administration and 2 root persistence tests |
| `crates/duduclaw-updated/src/linux/emergency.rs` | New exact-digest emergency preparation, cross-snapshot reconciliation and root commit-boundary test |
| `crates/duduclaw-updated/src/state.rs` | Accepted-time retries, actual emergency running version, monotonic floor, public Resume gate and crash tests |
| `crates/duduclaw-updated/src/linux/service.rs` | Connect real activator, self-restart continuation, all-boot health, scratch cleanup and generation GC |
| `crates/duduclaw-updated/src/linux/launcher.rs` | Revalidate historical accepted slots, resume trial staging, authenticated emergency fallback |
| `crates/duduclaw-updated/src/linux/files.rs` | Bounded safe directory cleanup/free-space checks, real filesystem persistence failpoints and same-fd exec tests |
| `crates/duduclaw-updated/src/linux/download.rs` | Explicit real/effective/saved uid/gid readback and downloader root acceptance test |
| `crates/duduclaw-updated/src/linux/mod.rs` | Export Linux activation, trust and emergency adapters |
| `crates/duduclaw-updated/src/lib.rs` | Export portable seam and trust ledger |
| `crates/duduclaw-updated/src/main.rs` | Root-only `--rotate-root` and `--emergency` commands |
| `crates/duduclaw-updated/README.md` | Document activation prerequisites and root test entry point |
| `scripts/linux-protected/updated/duduclaw-updated.service` | Fixed-service ordering and capabilities needed for authentic live readback |
| `scripts/linux-protected/auto-upgrade/README.md` | Update integration boundary description |
| `CHANGELOG.md` | User-facing round 2 behavior and remaining rollout gates |
| `docs/architecture/native-linux-isolation.md` | Current seams, recovery, commands, results, and archived round 1 handoff |
| `docs/features/05-security-defense.md` | English update behavior and acceptance boundary |
| `docs/features/zh-TW/05-security-defense.md` | Traditional Chinese update behavior and acceptance boundary |
| `docs/features/ja-JP/05-security-defense.md` | Japanese update behavior and acceptance boundary |
| `docs/guides/linux-protected-service.md` | English activation/recovery description |
| `docs/guides/zh-TW/linux-protected-service.md` | Traditional Chinese activation/recovery description |
| `docs/guides/ja-JP/linux-protected-service.md` | Japanese activation/recovery description |
| `wiki/impl/native-linux-isolation-package-f-2026-11-05.md` | Removed after moving original evidence below; no Package F security-gap handoff remains in wiki |

### Archived round 1 handoff (superseded by round 2)

The following original evidence was moved out of the published wiki. Its remaining-work list and counts describe round 1 only.

<details>
<summary>Round 1 evidence</summary>

### Package F implementation handoff — 2026-11-05

Status: **PARTIAL**. Work is confined to the assigned existing worktree and branch
`feat/iso-f-20261105`. No commit, branch switch, merge, release, deployment or
customer-machine action was performed. The new small crate compiles; the full
package is not ready to enable. Gateway/CLI/web compilation remains unverified.

#### Implemented

- Strict canonical metadata and Rust OpenSSH sshsig Ed25519/SK-Ed25519 verification,
  root 2/3 distinct-key thresholds, dual-threshold rotation verification,
  role/application/UP+UV rules, expiry/freeze/revocation and emergency verification.
- Official raw-artifact download under duduclaw-dl; root-owned bounded staging,
  openat2/dirfd checks, same-fd hash/exec, durable state, bounded RPC history,
  monotonic committed floor plus failed-attempt serial high-watermark.
- A/B selector/first-boot rollback state machine, frozen launcher, inactive-slot
  installation helper, and first real root-run self-tests before healthy marking.
- Dashboard Admin Apply/Status, retained request IDs on ambiguous results,
  localized progress and committed-only success; protected CLI requester and
  replacement nightly requester.
- Release manifest signer, real OpenSSH software-key fixtures (no private keys),
  unit tests, Linux privileged test entry point, and documentation.

#### Required remaining implementation

1. Replace `UnavailableActivator` with genuine drain/current-generation switch,
   fixed-unit restart and live product/version/build/protocol readbacks. Wire
   `stage_inactive_slot` and continuation after updater self-restart. Today the
   daemon verifies/stages a release, then reports Maintenance without switching
   current or committing the floor.
2. G provisioning: independently verified bootstrap program/package, BOTH migration
   signatures, pinned trust, initial state/slots/accounts and unit installation.
   `verify_bootstrap_root` only handles the compiled root fingerprint + threshold;
   no production root fingerprint or keys are provided.
3. Automatic recovery/cleanup for interrupted root `pending-generation` staging,
   prepared transactions, disk budgets and generation garbage collection. Current
   partial staging is retained and requires root inspection; it is not a completed
   crash-resume implementation for every filesystem point.
4. Root rotation/freeze/revocation persistence and feed/commands, accepted historic
   key material across target rotation; emergency installer with persisted serial
   and signed history. Current crypto APIs only verify these documents.
5. Runtime-entry publication, required data migrations and Pro release feed:
   current implementation explicitly refuses them.
6. Real FIDO2 fixtures and hardware verification; SK ECDSA support if needed by
   GoTrust. SK Ed25519 currently has synthetic crypto vectors only.
7. Linux root/systemd acceptance, all three supported distributions, actual
   downloader IDs and descriptor lifetime, reboot/power-cut A/B and update lifecycle.
8. Remote gateway/CLI/web builds and tests, then independent Daybreak review.
   Ordinary independent code review was performed, but no Daybreak model is
   available in this session; it is not claimed as security acceptance.

The exact interfaces, root filesystem layout and limits are recorded in
[the Package F architecture note](#package-f).

#### Commands actually run

| Command | Actual result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated -p duduclaw-isolation --offline` | 49 passed (20 updater + 29 unchanged contract), 0 failed, 1 ignored real FIDO2 test; doc/bin suites 0 tests |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --offline` | Passed |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --tests --offline` | Passed after correcting one test-only Path::set_permissions compile error; 13 expected unsafe-boundary warnings |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-updated --offline` | Passed; no linker or Windows native execution |
| `python3 -m unittest discover -s scripts/tests -p 'test_sign_isolation_manifest.py' -v` | 4 passed; includes actual ssh-keygen sign and verify |
| `python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v` | 12 passed, 1 skipped because systemd-analyze is unavailable on this Mac |
| `git diff --check` | Passed |
| Python JSON parse of `web/src/i18n/{zh-TW,en,ja-JP}.json` | 3 passed |
| Scan of new crate for `BEGIN .*PRIVATE KEY` | No matches; fixtures contain public keys and signatures only |

Earlier focused updater test runs also passed (11, then 19 active tests as state
coverage was added). Final totals above supersede those intermediate runs.
No cargo fmt, gateway/CLI/workspace build, dependency installation, native Linux
execution or web dependency installation was performed. `web/node_modules` is
absent, so the specifically authorized conditional tsc/vitest checks were skipped.

#### Builder commands and filters still required

```sh
### Linux: includes non-root seqpacket/artifact adapter tests that macOS cannot run.
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated
### Root-capable Linux: root dirfd/permissions/atomic replacement.
cargo test -p duduclaw-updated -- --ignored iso_root_
### Real hardware, only after creating the documented fixtures:
cargo test -p duduclaw-updated real_fido2_sk_ed25519_openssh_vector -- --ignored
cargo test -p duduclaw-gateway protected_update_
cargo test -p duduclaw-gateway restart_pending_tests
cargo test -p duduclaw-cli service_protected_
cd web
npx tsc -b
npx vitest run src/lib/protected-update.test.ts src/components/settings/sections/UpdateTab.test.tsx
```

The root test presently covers filesystem trust/atomic replacement only; the full
A/B reboot, downloader uid and native systemd integration harness is still needed.

#### CONTRACT CHANGE

None. No shared contract source was changed; all 29 contract tests remain green.
Package F defines the previously unspecified signed-file JSON representation in
its own metadata module. New crate dependencies were already locked packages:
serde, serde_json, thiserror, base64, sha2, ed25519-dalek, hex, nix, libc, tempfile.
Cargo.lock adds only the workspace crate and dependency edges, no external version
upgrade or new external package.

#### Review findings addressed

Independent readers reviewed actual code. Fixes include matching signer/server
signature-array formats; preserving unknown-outcome tracking; delaying healthy
until account/listener readiness; persisting maintenance even when boot refuses;
retaining failed serial high-watermark; idempotent healthy-slot staging; separating
update freeze from installed-slot authenticity; and poisoning all persistence
writes after an uncertain post-rename failure. A regression proves that even when
the new floor landed before fsync failed, later error handling cannot overwrite it
with the old in-memory floor. Policy refusals do not poison the writer.

#### Files changed or added

- `CHANGELOG.md` — Add a self-contained zh-TW partial implementation entry.
- `Cargo.lock` — Record the new local crate and dependency edges.
- `Cargo.toml` — Register the updater workspace member and dependency.
- `crates/duduclaw-cli/Cargo.toml` — Declare internal updater dependencies (new crate reuses already locked external packages).
- `crates/duduclaw-cli/src/service/protected_linux_install.rs` — Replace protected direct install with version-only daemon request.
- `crates/duduclaw-cli/src/service/protected_linux_tests.rs` — Stable-version and operator guard regression tests.
- `crates/duduclaw-gateway/Cargo.toml` — Declare internal updater dependencies (new crate reuses already locked external packages).
- `crates/duduclaw-gateway/src/handlers/dispatch_system.rs` — Admin authorization for protected update status.
- `crates/duduclaw-gateway/src/handlers/mod.rs` — Register the protected update handler module.
- `crates/duduclaw-gateway/src/handlers/protected_update_rpc.rs` — Admin dashboard updater request/status bridge and regression tests.
- `crates/duduclaw-gateway/src/handlers/system_config_rpc.rs` — Route protected apply requests to the root service.
- `crates/duduclaw-gateway/src/updater.rs` — Expose protected update channel and operator explanation.
- `crates/duduclaw-updated/Cargo.toml` — Declare internal updater dependencies (new crate reuses already locked external packages).
- `crates/duduclaw-updated/README.md` — Document purpose, usage constraints and validation requirements.
- `crates/duduclaw-updated/src/client.rs` — Fixed authenticated socket client and request-id binding.
- `crates/duduclaw-updated/src/lib.rs` — Portable module API, strict version parser and safe error codes.
- `crates/duduclaw-updated/src/linux/download.rs` — Fixed official HTTPS assets downloaded after uid/group drop.
- `crates/duduclaw-updated/src/linux/files.rs` — Root dirfd storage, atomic snapshots, fd hashing and root tests.
- `crates/duduclaw-updated/src/linux/launcher.rs` — Same-fd slot exec, trial persistence, staging and bootstrap pin helper.
- `crates/duduclaw-updated/src/linux/mod.rs` — Linux adapter exports and root process hardening.
- `crates/duduclaw-updated/src/linux/service.rs` — Authenticated root daemon, signed staging and unavailable activator seam.
- `crates/duduclaw-updated/src/linux/transport.rs` — Bounded seqpacket, credentials, ancillary rejection and tests.
- `crates/duduclaw-updated/src/main.rs` — Linux launcher/service/nightly entry points; other platforms refuse.
- `crates/duduclaw-updated/src/metadata.rs` — Canonical signed JSON encoding and ambiguity rejection.
- `crates/duduclaw-updated/src/sshsig.rs` — Strict SSHSIG Ed25519 and SK Ed25519 verification and vectors.
- `crates/duduclaw-updated/src/state.rs` — Durable floor/journal/slot/request state and failure matrix tests.
- `crates/duduclaw-updated/src/trust.rs` — Root threshold/rotation, release/installed-slot and emergency verification.
- `crates/duduclaw-updated/tests/fixtures/README.md` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1-release.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1-root.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1-wrong.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1.pub` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2-release.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2-root.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2-wrong.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2.pub` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/message.json` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `docs/README.md` — Index the new architecture handoff.
- `docs/architecture/native-linux-isolation.md` — Package F interfaces, semantics, boundaries and remaining work.
- `docs/features/05-security-defense.md` — Describe actual protected update routing without claiming isolation or delivery acceptance.
- `docs/features/ja-JP/05-security-defense.md` — Describe actual protected update routing without claiming isolation or delivery acceptance.
- `docs/features/zh-TW/05-security-defense.md` — Describe actual protected update routing without claiming isolation or delivery acceptance.
- `docs/guides/ja-JP/linux-protected-service.md` — Document new requester and development-only activation boundary; preserve legacy guidance.
- `docs/guides/linux-protected-service.md` — Document new requester and development-only activation boundary; preserve legacy guidance.
- `docs/guides/zh-TW/linux-protected-service.md` — Document new requester and development-only activation boundary; preserve legacy guidance.
- `scripts/linux-protected/auto-upgrade/README.md` — Document purpose, usage constraints and validation requirements.
- `scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade` — Replace legacy upgrade/download shell with fixed launcher requester.
- `scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade.service` — Remove legacy write paths; bound requester service.
- `scripts/linux-protected/updated/duduclaw-updated.service` — Development-only root daemon unit template.
- `scripts/sign-isolation-manifest.py` — Canonical manifest creation and offline ssh-keygen signing.
- `scripts/tests/test_protected_auto_upgrade.py` — Requester and source-trust installer regression tests.
- `scripts/tests/test_sign_isolation_manifest.py` — Real OpenSSH signing-tool tests.
- `web/src/components/settings/sections/UpdateTab.tsx` — Request tracking, polling, terminal states and safe retry UX.
- `web/src/i18n/en.json` — Localized protected update progress/result copy.
- `web/src/i18n/ja-JP.json` — Localized protected update progress/result copy.
- `web/src/i18n/zh-TW.json` — Localized protected update progress/result copy.
- `web/src/lib/api.ts` — Type protected apply/status RPC responses.
- `web/src/lib/protected-update.test.ts` — Regression coverage for states and lost replies.
- `web/src/lib/protected-update.ts` — Portable request tracking and ambiguous-outcome handling.
- `wiki/impl/native-linux-isolation-package-f-2026-11-05.md` — This evidence and remaining-work report.

</details>
