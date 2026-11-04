# duduclaw-egress

Host proxy for isolated Linux employee runs. It accepts only root-registered
socketpairs, pins each connection to its run and root-published profile, and
opens TCP tunnels only to public IP addresses on approved ports. It does not
filter tunnel content or enforce a domain allowlist.

This package is not yet connected to the gateway or installed by the product.
Deployment requires spawnd, the namespace trampoline, a root-owned profile file,
a systemd socket and service, and either resolved Varlink or the dedicated DNS
broker. Missing dependencies fail closed; there is no direct-network fallback.

## Build and test

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-egress
```

Host mode consumes systemd fd 3 at the contract's control socket. The separate
forwarder mode consumes a preconnected stream fd 3 and serves CONNECT on the
namespace's loopback. Both use the unchanged `duduclaw-isolation` v1 contract.
B1 integrates through `controller::EgressController`; `RootEgressController`
registers socketpairs and waits for cleanup acknowledgements. Missing wiring
uses `UnavailableEgress`, which denies every operation. Cancellation or timeout
closes the controller and revokes all its runs. Root acceptance commands and
fixture requirements are documented in the implementation note.
See [implementation notes](../../docs/architecture/native-linux-isolation.md)
for deployment prerequisites, API seams and unverified acceptance gates.

## License

Apache-2.0, as declared by the workspace.
