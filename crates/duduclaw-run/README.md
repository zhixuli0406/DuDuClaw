# duduclaw-run

Linux employee-process launcher for the native isolation program. It validates
root-delivered descriptors, builds a private filesystem and namespaces, drops
privileges, and applies Landlock and seccomp before executing a registered ELF.
Scripts use a registered interpreter and an identity-checked read-only script.

This package is not connected to the gateway. The executable refuses execution
until package B1 supplies the `RootRegistry` bootstrap adapter. A missing adapter
never executes a command under the gateway or root identity. The library exposes
the launcher to that trusted adapter; no public request accepts argv, paths, uid,
environment, mount choices or a seccomp profile.
The B1 adapter must enforce systemd `KillMode=control-group` and kill the run
cgroup on bootstrap failure, EOF or timeout. Standalone launch is unsupported.

## Build and test

Requires Rust (workspace edition 2024). Native execution requires Linux 5.15 or
newer with Landlock ABI 1–7, seccomp, and root in a systemd transient service with
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

## B1 adapter

Implement `linux::bootstrap::RootRegistry::take_run` using the verified root
registry. B2 provides `receive_challenge`, `send_offer`, and `receive_status` for
the root sender. `status::StatusRecord` checks exact framing, reserved bytes,
bounded values and ancillary counts; it does not authenticate the sender or
verify a PTY's mount identity. The startup challenge authenticates the root peer
before privilege drop. Retain that same connected endpoint for status delivery.
Poll that endpoint with B1's startup/runtime deadline before receiving each
record: the five-second socket I/O timeout is not the running task's deadline.
Never treat `ChildrenForked` as proof of exec or protected-mode readiness.

## License

Apache-2.0, as declared by the workspace.
