# duduclaw-parse-worker

Parses untrusted documents in a disposable Linux service instead of the gateway
account. A systemd socket starts one DynamicUser service per connection. Requests
carry sealed input file descriptors and the shared isolation worker contract;
responses are bounded structured documents/tables/HTML, image dimensions, archive
manifests and verified output artifacts, or closed errors.

The default library contains only the client, policy and transport. Parser
dependencies are behind `engine`, required by the binary. Install the socket and
template under `systemd/` as root, and install the binary at the template's fixed
ExecStart path. These files are deployment inputs, not an installer. Do not grant
the gateway permission to create arbitrary systemd units.

The A/G integration seam is `client::require_isolation()` before accepting any
work; the root-owned gateway unit may also set `DUDUCLAW_ISOLATION_REQUIRED=1`.
Presence of that variable (even empty or malformed) requires isolation. The latch
cannot be reset. This is not a protected-readiness check or a migration switch.
Unsupported operations fail closed; legacy installations retain their old path.

The named A/B lifecycle seam is `WorkerLauncher` / `WorkerSession`, installed once
through `install_launcher`. The default fixed socket launcher has process-local
admission and failure quarantine, but does not provide host-wide cancellation
receipts. Output memfds are sealed before size and SHA-256 verification.

## Development

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-parse-worker --features engine --offline
cargo build -p duduclaw-parse-worker --features engine
```

Privileged acceptance tests have the prefix `iso_root_` and are ignored by default.
Run them on a disposable Linux system with the supplied units installed. See
`docs/architecture/native-linux-isolation.md` for coverage and remaining seams.

## License

Apache-2.0.
