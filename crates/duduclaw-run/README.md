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

B1's readiness interface carries detected/selected fields. Once a trusted
installation producer supplies verified evidence, the daemon logs the pair once
per daemon process and writes `EvidenceReport.landlock` for operator readback.
The production producer remains `MissingInstallation`; a successful ABI probe
alone cannot fill this gap. The fixed bootstrap privilege receipt reports only
the selected ABI (1–7), precedes rule enforcement and does not prove successful
exec. No protection from unknown future rights is claimed. Native Linux
re-validation and complete installation acceptance are still required.

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
