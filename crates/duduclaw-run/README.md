# duduclaw-run

Linux employee-process launcher for the native isolation program. It validates
root-delivered descriptors, builds a private filesystem and namespaces, drops
privileges, and applies Landlock and seccomp before executing a registered ELF.
Scripts use a registered interpreter and an identity-checked read-only script.

The executable now uses `linux::sealed_registry::SealedRegistry` to receive B1's
root-authorized plan. B1's native adapter supplies a sealed memfd manifest bound
to the authenticated bootstrap challenge, nonce and run identity, followed by
the declared descriptors. The receiver checks ownership, seals, size, exact
framing and the one-shot binding; it accepts no gateway-selected argv, paths,
uid, environment, mount policy or seccomp profile. Missing or invalid authority
never executes a command under the gateway or root identity.

The transport is wired, but protected TEAM execution remains closed until the
trusted root publication, installation/readiness producer and other components
are available. This is not a deployable or accepted protected installation.
The B1 adapter must enforce systemd `KillMode=control-group` and kill the run
cgroup on bootstrap failure, EOF or timeout. Standalone launch is unsupported.

## Build and test

Requires Rust (workspace edition 2024). Native execution requires Linux 5.15 or
newer with Landlock ABI 1 or later, seccomp, and root in a systemd transient service with
the design's capabilities and private devices. macOS/Windows build only the pure
policy and validation logic and return an unsupported-platform refusal.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-run --target x86_64-unknown-linux-gnu --offline
```

Privileged tests are opt-in, isolated subprocesses and use disposable paths:
`cargo test -p duduclaw-run --features root-test-fixture -- --ignored iso_root_ --test-threads=1`.
They cover the complete `serve` path using a test-only root registry, including
ELF/script execution, PTY and descriptor handoff, failures, signal forwarding,
PID namespace teardown and late-mount propagation. The fixture cannot compile
in release mode. Its runtime syscall list is a test profile, not an approved
production runtime profile. Actual B1 transient units remain an integration gate.
See [implementation notes](../../docs/architecture/native-linux-isolation.md)
for integration boundaries, status records and remaining acceptance gates.

## Landlock compatibility

Kernel ABIs above 7 select exactly the ABI-7 filesystem ruleset. ABI 0 or an
unavailable API still refuses execution; failed rule installation never falls
back. `linux::probe::Readiness` keeps both the actual `landlock_abi` and selected
`landlock_ruleset_abi`. The probe does not print those values to employee stderr.

B1's readiness interface carries detected/selected fields. Once verified
evidence exists, the daemon logs the pair once per daemon process and writes
`EvidenceReport.landlock` for operator readback. A successful ABI probe alone
is never that evidence: since R7a the producer is spawnd's own self-test
(below), not an installer receipt. The fixed bootstrap privilege receipt reports only
the selected ABI (1–7), precedes rule enforcement and does not prove successful
exec. No protection from unknown future rights is claimed. Native Linux
re-validation and complete installation acceptance are still required.

## Isolation self-test (`duduclaw-run-selftest`, R7a)

A second shipped binary of this crate (`src/bin/selftest.rs`, built in release
with the trampoline, signed as `InstallTarget::TrampolineSelftest` and
installed at `/usr/lib/duduclaw/duduclaw-run-selftest`). It is not a mode of
`duduclaw-run`, which still refuses any argv but its own name. It accepts no
argument, reads no environment or stdin and closes every fd above 2. Only PID 1
runs it, through the fixed unit
`scripts/linux-protected/spawnd/duduclaw-isolation-selftest.service`, which
spawnd starts after its start-up recovery.

It runs this crate's own library code in the trampoline's order: `probe`,
`require_root_capabilities`, `enter_namespaces` (then reads
`/proc/thread-self/mountinfo` to confirm no mount keeps a shared/slave peer),
a private tmpfs work area, `build_root` + `pivot` + `bring_loopback_up`, then
`unshare(CLONE_NEWPID)` and one forked child that is that namespace's init:
`mount_proc`, `privileges::drop_all` to the dedicated `duduclaw-selftest`
account (refused if root, an employee uid/gid or any service account's uid/gid),
`runner::restrict_filesystem` (the `RUNTIME_LANDLOCK` table) and
`seccomp::common`. It then asserts: ordinary file write/read in `/work` and
`/home/employee` succeeds (positive control); a pathname socket in `/tmp`
succeeds; a pathname socket and a FIFO in `/work` and `/home/employee` fail
with `EACCES`; `/etc/hosts` reads; opening `/` (no rule covers it) fails with
`EACCES`. Exit 0 prints one stdout line
`{"version":1,"kernel":[major,minor],"detected_abi":N,"selected_abi":M}`;
any failure prints one stderr line `{"code":"selftest_failed","stage":"<name>"}`
and exits with the stage's code (2 descriptors, 3 arguments, 4 capabilities,
5 probe, 6 account, 7 namespaces, 8 propagation, 9 work_area, 10 root, 11 pivot,
12 loopback, 13 pid_namespace, 14 supervise, 15 output, 20 child_pid,
21 privileges, 22 landlock, 23 seccomp, 24 positive_write, 25 socket_allowed,
26 socket_denied, 27 fifo_denied, 28 read_allowed, 29 read_denied; 125 on
non-Linux). It does not exercise the bootstrap socket, detached mounts made by
spawnd, `/proc` `hidepid`/`subset` visibility, the PTY path, the supervisor or
runtime seccomp filters, or exec.

Stage 4 `capabilities` also fixes the capability state to the run units'
exactly: bounding = effective = permitted = `REQUIRED_BOUNDING_CAPABILITIES`
(CAP_CHOWN CAP_SETGID CAP_SETUID CAP_SETPCAP CAP_NET_ADMIN CAP_SYS_ADMIN
CAP_SYS_RESOURCE), inheritable and ambient empty, read back. Under the fixed
unit the bounding set already is that set, and an effective set holding
CAP_DAC_OVERRIDE or CAP_DAC_READ_SEARCH fails the stage. Started with a wider
bounding set (by hand as full root) it lowers the bounding set first, so every
later stage, `build_root` included, runs without either DAC capability, as the
trampoline does under its unit. The work area's `home` and `work` are what
spawnd hands the trampoline: mount roots owned by the account, mode 0700.

Root-only tests (`DUDUCLAW_ISO_DISPOSABLE=1`, creates the `duduclaw-selftest`
system account if missing; no `root-test-fixture` feature needed; one test
runs the binary as a transient `systemd-run` service with
`NoNewPrivileges=yes` and the run units' `CapabilityBoundingSet=`):
`cargo test -p duduclaw-run --test selftest_root -- --ignored iso_root_ --test-threads=1`.

## B1 adapter

B1's `native_trampoline::NativeTrampoline` uses the sealed manifest transport;
B2's production `SealedRegistry::take_received_run` consumes it exactly once.
The legacy `take_run` lookup remains unavailable in that implementation. B2 also
provides `receive_challenge`, `send_offer`, and `receive_status` for the root sender. `status::StatusRecord` checks exact framing, reserved bytes,
bounded values and ancillary counts; it does not authenticate the sender or
verify a PTY's mount identity. The startup challenge authenticates the root peer
before privilege drop. Retain that same connected endpoint for status delivery.
Poll that endpoint with B1's startup/runtime deadline before receiving each
record: the five-second socket I/O timeout is not the running task's deadline.
Never treat `ChildrenForked` as proof of exec or protected-mode readiness.

## License

Apache-2.0, as declared by the workspace.
