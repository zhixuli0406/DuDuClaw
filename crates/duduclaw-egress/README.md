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

The dedicated DNS broker sends exactly one A and one AAAA query to its fixed
upstream over TCP; it never follows aliases with extra queries. Each supplied
CNAME/DNAME chain must start at the requested name and remain consistent.
An alias chain without an address of the queried type is accepted as an empty
family result only for NOERROR with TC clear and a valid IN SOA in AUTHORITY
whose owner equals or is an ancestor of the final alias target. Alias-only
answers without that SOA, unrelated SOAs, malformed replies and NXDOMAIN are
refused. Both families empty still fails; addresses remain subject to the
existing downstream public-IP policy.

## Global admission limit

The daemon shares 64 tunnel permits across all registered runs. It acquires a
permit before spawning a DNS/connect task and retains it until the tunnel closes;
there is no per-tunnel wait queue. Saturation returns `ResourceLimit` without DNS
or TCP I/O. Cancellation waits for child futures to drop and return their permits.
Configuration also rejects `max_runs` times any profile's `max_tunnels` above
1024, bounding the possible per-run tunnel bookkeeping independently of the
active global ceiling.

## Update readback endpoint (v1.83 R6)

Before installing seccomp, the host service hashes its running image
(`/proc/self/exe`) and binds `/run/duduclaw-egress-update/readback.sock` beneath
verified descriptors (root-owned `/run`; the unit's `RuntimeDirectory=` leaf,
owned by `duduclaw-egress`, mode 0750, bind-mounted through
`TemporaryFileSystem=/run:ro`). Only a uid 0 / gid 0 peer is answered: one
`Readback{generation}` frame in, one `{target: egress, artifact_id, product,
version, protocol, build_sha256, effective_uid, ready}` record out; no ancillary
data, unknown fields refused, 5 s per exchange. `ready` is true only after the
startup checks and seccomp, and only when every field was computed. Without the
endpoint egress still runs; the updater's readback then fails closed.

## License

Apache-2.0, as declared by the workspace.
