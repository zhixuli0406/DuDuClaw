# Installed isolation chain acceptance

These ignored Linux tests require an already provisioned, disposable root VM.
They fail when prerequisites are missing; they neither create signed acceptance
records nor relax production readiness. They have been cross-type-checked, not
executed on a root VM in integration I1 round 5.

## Claude transport

`claude_chain::iso_root_e2e_claude_installed_shim_tool_transport` exercises the
installed main plan factory, A's actual authenticated spawnd connection, B1's
transient unit, B2 and the real signed Claude ELF. Claude starts D's signed MCP
stdio shim using the installed read-only config and invokes a single test-only
`acceptance_echo` callback behind D's live connection verification.

The fixture authority has a deliberately named `transport-test-only` principal;
it checks A's live root Status and original control connection. It is **not** the
production UserDb authority, production CLI dispatcher, gateway boot, all-tool
factory completeness or product-tool acceptance. Those production gates remain
unchanged. This test consumes an actual model call through the installed egress
and provider configuration.

Prerequisites:

- Actual `duduclaw` gateway UID/GID and an idle, confirmed permanent employee with
  H-backed storage and exactly one entitled MCP-enabled Claude profile.
- Live F/B1/H/egress services, committed signed generation and continuously renewed
  installation/publication receipts. Exact runtime/bridge/native-policy acceptance
  must already exist; no test-generated success flags are accepted.
- The signed Claude fixed argv must exactly match
  `gateway_plan::claude_arguments(model)`, including text stdin, stream-json,
  `/proc/self/fd/5` system prompt and `/config/mcp.json` exclusive MCP config.
  The model is read from that signed argument list. The test does not inject keys.
- The copied integration-test executable must be traversable/executable by
  `duduclaw`. Root re-execs its internal helper as that UID/GID with no additional
  groups; root itself cannot authenticate to B1 as the gateway.

The user challenge is sent both as sealed fd 4 and Claude stdin. An independent
system challenge occurs only in the sealed fd 5 system prompt; the tool call must
supply both. D returns a fresh answer plus the system challenge, and the CLI output
must contain that result. While the callback waits, the root observer checks the
actual Claude and shim ELF hashes, all four UID/GID values, PID/mount/network/IPC/
UTS namespaces, exact run cgroup, shared MCP socket identity and sealed inputs.
It enumerates live descriptors as evidence; it does not claim that a running CLI,
which opens its own files, has only the initial exec whitelist.

Build on the Linux builder, not the shared development Mac:

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --test claude_chain --no-run
```

Run from a root shell on the installed disposable VM:

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --test claude_chain iso_root_e2e_claude_installed_shim_tool_transport -- --ignored --exact --test-threads=1 --nocapture
```

## D8 admission and double-fork residue

`d8_chain::iso_root_e2e_d8_double_fork_residue_blocks_second_run` exercises two
real A/B1/B2 runs for the same employee. It uses an explicitly registered fixture
ELF, not Claude or a production tool. The payload genuinely double-forks and
waits until its grandchild is adopted by the run's namespace PID 1.

Additional prerequisites:

- Build with `--features root-test-fixture`. The
  `duduclaw-spawnd-d8-fixture` binary deliberately refuses release-profile builds;
  never place it in production delivery manifests.
- The acceptance VM's separately signed test generation must register the exact
  compiled ELF under `RuntimeKind::Registered("iso-d8-fixture-v1")` and grant one
  idle employee that profile. The test verifies its current build digest against
  the signed runtime publication. Register empty fixed argv, `mcp: false`,
  `pty_allowed: false`, offline networking, queue timeout at least 30 seconds and
  runtime timeout at least 60 seconds. Resource/storage constraints remain the
  real root policy, and its architecture-specific seccomp profile must support
  the fixture's ordinary Rust I/O, fork/wait, setsid, PR_SET_NAME and pause calls.
  Common syscall prohibitions still apply; do not supply a permissive fake receipt.
- The copied fixture ELF must exist at the builder's absolute path embedded by
  `CARGO_BIN_EXE_duduclaw-spawnd-d8-fixture`; the existing `exes.txt`/`push.sh`
  convention preserves that path.

Root locates the first run's main, adopted orphan and namespace init from the
actual cgroup, checks their UID/namespaces and fixture digests, then uses a pidfd to
pause **only that run's init**. The gateway asks the main to exit; root observes
its exited pidfd/zombie while the orphan remains alive. For one second, the second
run must remain Queued, expose no I/O, and refuse a same-employee wait with
`WouldDeadlock`. Root then resumes init. When the second isolated process is
observed, the test requires the first cgroup empty and the orphan's pidfd exited. The
root observer then independently requires the first unit to be inactive and scans
all host process UID fields: any employee UID process must belong to the second
cgroup. These are repeated queue checks during the deliberately held interval
and post-release readbacks when the second process is observed, not a continuous
trace of every admission transition.

The stop/resume is a test-only scheduling aid to make residue reliably observable;
it does not modify B1 admission, B2 teardown, shared IPC or production policy.
Cleanup resumes the held init, closes the gateway child, cancels only the two
recorded test run IDs and waits with finite deadlines for retirement.

```sh
# Linux builder, including the exact fixture artifact to sign and install:
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --features root-test-fixture --test d8_chain --no-run
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo build -p duduclaw-spawnd --features root-test-fixture --bin duduclaw-spawnd-d8-fixture
# Root disposable VM, after independently provisioning the signed fixture:
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --features root-test-fixture --test d8_chain iso_root_e2e_d8_double_fork_residue_blocks_second_run -- --ignored --exact --test-threads=1 --nocapture
```

## Existing acceptance runner

Change the builder's spawnd `test --no-run` invocation to include
`--features root-test-fixture`; collect both integration-test executables and the
fixture binary into `exes.txt` and copy them with `push.sh`. As usual, keep
`ISO_EGRESS_ECHO_ADDR` set for the runner, even for the offline D8 test.

```sh
runall.sh /var/tmp/iso-i1-r5-chain 'iso_root_e2e_(claude_installed_shim_tool_transport|d8_double_fork_residue_blocks_second_run)$'
```

Exactly two outer tests should be selected and each must report `1 passed`.
The internal helpers intentionally lack the `iso_root_` prefix and must never be
run independently. Missing signed prerequisites cause failures, not skips.

## H4 work broker chain

`work_broker_chain::iso_root_work_broker_chain_backfill_snapshot_and_detach` uses
real H loop storage (no systemd unit, no trampoline): a pre-H3 allocation gets
its anchor from the start-up backfill exactly once; a lease is attached, the
root-held RO broker clone is taken, an employee-owned file is read through the
real `verify_lease` and a sealed snapshot; while a copy still holds the clone H
cannot finish the detach, and after `BrokerRoot::release` the same detach
succeeds; the snapshot outlives the detach; retire removes the anchor. Needs a
root disposable VM with `ISO_STORAGE_TEST_ROOT` on ext4/XFS (loop devices) and
runs in a private mount namespace of its thread.

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --test work_broker_chain --no-run
DUDUCLAW_ISO_DISPOSABLE=1 ISO_STORAGE_TEST_ROOT=/srv/iso-storage cargo test -p duduclaw-spawnd --test work_broker_chain -- --ignored --test-threads=1 --nocapture
```

## I1e installed PTY chain

`pty_chain.rs` adds three `iso_root_pty_` tests using the real A backend, original
B1 control connection, systemd and production B2. Run serially on a disposable
root Linux VM with one confirmed **idle** employee. Provision the debug-only
`duduclaw-spawnd-pty-fixture` ELF as signed registered runtime
`iso-pty-fixture-v1` with empty fixed argv, offline network, `mcp: false`,
`pty_allowed: true`, an idle budget of 2–10 seconds and a runtime budget greater
than four idle periods. Preserve actual F/H/account/namespace readiness. The
signed syscall profile must support ordinary Rust I/O, termios ioctls, getsid,
getuid/geteuid and fcntl, without widening B2's common prohibitions. The root
observer checks the exact currently built fixture digest; copy the binary to
its embedded `CARGO_BIN_EXE` path on the VM before running. No test creates or
signs policy, bypasses readiness, allocates an account, or skips missing setup.

```sh
# Native Linux builder:
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --features root-test-fixture --test pty_chain --no-run
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo build -p duduclaw-spawnd --features root-test-fixture --bin duduclaw-spawnd-pty-fixture
# Root disposable acceptance host, after signing/installing the exact fixture:
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --features root-test-fixture --test pty_chain iso_root_pty_ -- --ignored --test-threads=1 --nocapture
```

Exactly **3 outer tests** must pass. The internal `installed_pty_gateway_child`
helper is selected only by the root parent; it verifies the root observer's
socket identity and runs as the real gateway UID with no supplementary groups.
The tests cover terminal FD identity/namespaces, one-shot master delivery,
initial 31×113 dimensions read at startup before any dynamic resize, 37×93
resize read back by the employee, input/output, real-byte idle renewal followed
by idle expiry, cancellation, and independent unit/cgroup/UID retirement.
They do not exercise authenticated product ingress, Grok or any live provider.
