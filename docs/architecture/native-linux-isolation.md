# Native Linux isolation implementation

Implementation notes for the r7 design. These packages are not enabled by the
existing gateway. Building a package does not establish protected-mode readiness.

## Package C — egress proxy

`crates/duduclaw-egress` supplies the host daemon, namespace CONNECT forwarder,
and a separate DNS-only broker mode. Production OS operations are Linux-only;
address policy, CONNECT parsing, budgets and environment construction are portable.
The shared isolation contract is unchanged. Dependencies already present in the
workspace lockfile are reused: Tokio, serde/serde_json, nix 0.29 and libc.

### Guarantee and limits

The guarantee is **no connections to non-public IPs**, with destination ports
restricted by the root-selected profile. This is not content filtering. Tunnels
carry plain TCP bytes (normally end-to-end TLS); no SNI, HTTP Host or domain
allowlist is enforced. Public relays remain reachable. A public endpoint proxying
a private service is outside this IP-destination guarantee. AF_VSOCK is neither
representable as a destination nor enabled in the service address-family policy.

The policy follows the [IANA IPv4 registry](https://www.iana.org/assignments/iana-ipv4-special-registry/)
and [IANA IPv6 registry](https://www.iana.org/assignments/iana-ipv6-special-registry/)
(2025-10-09 registry revision). Non-global, multicast, broadcast, loopback,
link-local, private, CGNAT and documentation ranges are denied. The implementation
also denies all mapped/compatible IPv4, NAT64, 6to4 and Teredo prefixes. IPv6
outside currently allocated global unicast `2000::/3` is denied conservatively.
Registry changes require code and table-test review; there is no runtime download.
Standard Rust IP parsing is used; ambiguous decimal/octal/hex/short IPv4 spellings,
zone IDs, URLs, userinfo, non-ASCII DNS labels and invalid DNS syntax are refused.
International names must arrive as ASCII punycode.

Every tunnel resolves afresh, rejects the entire result if any answer is not
public, and connects numeric `SocketAddr`s from that checked set. A connect never
resolves the hostname again. All retries remain within the pinned set. DNS and
connect together have the profile's deadline, and the connected peer is checked.

### Integration seams for A and B1/B2

The named B1 seam is `controller::EgressController` (object safe). Its concrete
`RootEgressController::connect()` is called inside a Tokio runtime by root
spawnd; it accepts only the fixed root:root 0600 control socket, checks protected
ancestors and root peer credentials, and has no configurable alternate path.
`register(run, profile)` creates the CLOEXEC socketpair itself, sends its host
end with SCM_RIGHTS, and returns the forwarder end only after a correlated
`Registered` acknowledgement. `unregister(run)` waits for `Unregistered` after
host-side cleanup. The shared v1 messages and hard limits are unchanged.

B1 retains the controller for its lifetime and calls unregister on every run
exit/cancellation. An I/O error, bad/extra-fd ACK, wrong request ID/direction,
five-second timeout or cancellation of a polled operation drops the control
socket and permanently poisons that client. This revokes **all** its runs; B1
must stop their runtime units and must not interpret reconnect as restoring old
capabilities. A valid correlated `Refused` response keeps the channel usable.
Dropping a future before polling it has no effect. `UnavailableEgress` is the
default implementation and always denies; no permissive fallback is provided.
The daemon also authenticates the peer at its `serve_control` integration entry.


- Root starts the host binary through the supplied systemd socket and service
  templates. The service uid/gid must be `duduclaw-egress`, with zero capabilities.
  The daemon verifies uid/gid, `PR_SET_DUMPABLE=0`, `NoNewPrivileges`, configuration
  ownership and socket activation. After bootstrap/probes it installs a native
  x86_64/aarch64 seccomp BPF allowlist with TSYNC; other architectures fail closed.
  Unknown syscalls return EPERM and a mismatched syscall architecture kills the
  process. Socket creation permits only AF_UNIX/INET/INET6. exec, clone, namespace
  creation and socketpair are refused. The socket is root-owned at the contract's
  `/run/duduclaw-egress/control.sock`, `SOCK_SEQPACKET`, mode 0600. It authenticates
  the controller's uid **and** gid as zero; PID never authorizes a request.
- B1 keeps one control connection for its lifetime. `ipc::Envelope<EgressControl>`
  uses the unchanged length-prefixed v1 seqpacket format. `Register` carries one
  root-created unnamed AF_UNIX stream socketpair endpoint via SCM_RIGHTS.
  Unknown fields/cmsgs, extra/missing fds, truncation, wrong socket type, named
  sockets and non-root creation credentials fail closed. Received descriptors
  are CLOEXEC and closed on rejection, including data-plane ancillary fds.
- B1 waits for `Registered` before releasing the forwarder. Binding lives in the
  host run table; frame payloads cannot choose a run, employee, profile or resolver.
  Duplicate run IDs are rejected for the controller lifetime. Profiles are loaded
  once from root-owned `/etc/duduclaw/egress.json`; unknown IDs are refused.
- B1 sends `Unregister` when a run ends. The host cancels in-flight DNS/connects,
  waits for tunnel tasks and descriptors to close, then sends `Unregistered`.
  Control EOF terminates every run, drains cleanup and exits the daemon; systemd
  restarts it. A restart does not restore old registrations. B1 must consider
  all prior capabilities revoked and register fresh runs only after readiness.
- B2 gives only the forwarder its egress socket as fd 3. Invoke the same ELF as
  `duduclaw-egress forwarder <port> '<EgressLimits JSON>'`. The optional JSON is
  bounded to 2048 bytes and is the root profile's limits, not authority from an
  employee. Omitting it uses `budget::default_limits()`. The forwarder binds
  `127.0.0.1`, accepts only CONNECT, returns 405 for other methods including
  absolute-form HTTP, and never retries outside the socket. The host enforces
  its own profile independently. The helper supports at most 16 simultaneous
  clients and 4096 lifetime tunnels, even if a profile permits more.
- A/B1 call `environment::child_environment(root_allowlist, Some(port))` and apply
  the result after `Command::env_clear()`. The input must already be a trusted
  allowlist, never the inherited process environment. All case variations of
  `*_PROXY` and `PROXY` are removed, then only upper/lowercase HTTP_PROXY and
  HTTPS_PROXY are set. `None` produces an offline map with no proxy variables.
  This helper does not sanitize arbitrary other environment variables. Namespace
  isolation, runtime registration and the offline/no-egress decision belong to B.

### Root configuration

`Config` is a strict JSON object with `profiles` (1–128 contract EgressProfile
objects), `run_caps` (`total_bytes`, `total_connections`) and `max_runs` (1–128).
No field has an implicit permissive default. A profile's digest is a root-published
registry key; the proxy does not invent a new canonical profile encoding.
`total_connections` is 1–65536; it counts all admitted connect attempts, including
DNS or connect failure, and is never refunded by tunnel closure. The byte cap
counts payload in both directions before it is forwarded; protocol headers are
bounded separately. `max_tunnels` is additionally capped at 256 per run to bound
queues, and profiles below a 256-byte frame limit are rejected.

Per-run token buckets enforce aggregate frames/second and bytes/second (one
second of burst). Violations close the run; the implementation does not promise
traffic shaping. Bounded queues provide backpressure; queue overflow closes the
run. The controller is capped at 100 requests/second and 10,000 registrations
per connection. The forwarder uses its smaller lifetime/client bounds too.

Tunnel IDs must strictly increase on a connection. Client Data before Connected,
unknown IDs, duplicate Connect or wrong-direction frames are fatal. Closing is a
bounded state: crossed Close is idempotent; after a host TCP EOF, already-sent
client data may be drained and charged until client Close, but never forwarded.
Both actors use bidirectional activity for idle timeouts. Once a frame's first
byte arrives, a separate deadline bounds the entire frame (slowloris protection).

One structured terminal audit record contains only run/profile IDs, aggregate
payload bytes, attempt count and a fixed error code. No hostnames, IPs, headers,
payload, credentials, prompts or parser diagnostics are logged. Ordinary EOF and
explicit cancellation currently share the `unavailable` terminal code. The unit
bounds journal rate; host-wide journal retention remains the installer's duty.

### DNS routes and systemd deployment

The root profile explicitly selects one route. There is no automatic runtime
fallback and no libc/NSS hostname lookup for destinations:

1. `resolved_varlink`: fixed AF_UNIX
   `/run/systemd/resolve/io.systemd.Resolve`, `ResolveHostname` family 0, DNS-only,
   no search, no synthesized answers, no cache. Requests and replies are bounded;
   unsupported/error/partial replies fail closed. This uses the
   [systemd v249 interface](https://github.com/systemd/systemd/blob/v249/src/resolve/resolved-varlink.c).
2. `dedicated_dns_broker`: fixed AF_UNIX
   `/run/duduclaw-dns-broker/resolve.sock`. Install a distinct non-root
   `duduclaw-dns` account and run `duduclaw-egress dns-broker` in its own unit.
   Root-owned `/etc/duduclaw/dns-broker.json` contains only
   `{"upstream":"<literal IP>:53"}` (bracket IPv6). The broker accepts only the
   egress uid, creates its own A and AAAA queries, and connects only that numeric
   upstream on TCP 53. No UDP or arbitrary qtype/packet forwarding is exposed.
   Both address-family queries must succeed; CNAME/DNAME without a final address
   in the response is refused. At most 32 DNS requests run concurrently.

Both routes verify the fixed socket's protected ancestors and service/root peer
uid; the daemon probes every configured route before accepting registrations.
These probes verify socket availability, not upstream DNS health. Broker socket
mode is root:egress-group 0660 in a root-owned directory. Its daemon must run as
a different uid from egress. DNS replies are limited to 64 KiB and 64 addresses.

The templates under `crates/duduclaw-egress/systemd/` are **not installed by this
change**. The egress template has an empty capability set, NNP, strict filesystem
protection, a fixed syscall allowlist, resource limits and AF_UNIX/INET/INET6 only.
Its private read-only `/run` exposes the control directory and, by default, only
the resolved socket. A broker installation must replace that DNS bind with the
broker socket/directory in a root-owned drop-in. Installations deliberately using
both routes must bind both. The broker has its own `/run` projection and syscall
allowlist. The installer supplies its exact upstream IP allow rule; the shipped
broker template denies all IPs until that drop-in is present.

**R7-M2:** never add the upstream resolver to the egress service's IPAddressAllow.
Only the separate broker receives that exception. Egress's IP rules form a second
layer for direct socket destinations, including metadata. They cannot inspect a
public relay or tunnel content. B1 still must read back actual unit properties
and cgroup BPF attachment; missing second-layer enforcement is reported separately
from the primary code policy. Neither those readbacks nor namespace construction
are implemented by this package.

### Validation and remaining acceptance

Round 2 supplies the controller implementation and eight ignored root tests.
The macOS session executed 9 egress portable tests and 29 unchanged contract
tests: all passed. Linux x86_64 and Windows checks compile all test targets;
they do not execute Linux code. No gateway/CLI/workspace build was run. No
new third-party package/version or shared contract change was made in either round.
The general independent code review is not the required Daybreak security review.

Final round 2 checks: macOS tests **38 passed, 0 failed**; Linux x86_64 and
Windows GNU `--all-targets` checks **passed**; tracked and all 23 new-file
whitespace checks **passed**; `git diff -- crates/duduclaw-isolation` is empty.
The first Linux check caught three `Gid::is_root` API errors in the new client;
these were changed to explicit numeric gid checks before the passing rerun.
The compiled Linux test inventory is 35 ordinary and 8 ignored test functions;
this inventory is not an execution result.

Normal commands (run the first command again on the Linux builder to execute
Linux-only controller, broker, forwarder, seccomp and host tests):

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-egress --all-targets --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-egress --all-targets --offline
```

The ordinary broker tests use real local TCP and Unix sockets: assert exact A
and AAAA queries, preserve private answers for host policy rejection, and refuse
partial-family failure and caller-supplied routes/methods/flags. DNS codec tests
cover CNAME/DNAME with a final address, alias-only refusal, NODATA, compression
and truncation. Alias-only answers are intentionally not followed by additional
queries. These are fixture results, not real upstream acceptance. Varlink flags
and reply fields were rechecked against systemd v249's
[implementation](https://github.com/systemd/systemd/blob/v249/src/resolve/resolved-varlink.c)
and [flag definitions](https://github.com/systemd/systemd/blob/v249/src/resolve/resolved-def.h).

Run root tests as root, **only on disposable acceptance VMs**, with serial test
execution. Missing prerequisites fail the tests rather than silently skipping:

| Test filter | Prerequisites / assertions |
| --- | --- |
| `iso_root_controller_register_revoke_and_disconnect` | No installed services or external network; actual root SCM_RIGHTS, two registrations, duplicate refusal, ACK-after-EOF and controller disconnect revocation. |
| `iso_root_unregister_ack_follows_data_socket_eof` | No installed services; raw wire-client regression for unregister ordering. |
| `iso_root_seccomp_real_syscalls` | Separate subprocess installs the actual TSYNC filter; denies VSOCK/netlink/packet sockets, user namespace, socketpair and writable open, while existing stream I/O, TCP and Tokio timers work. Does not install a filter in the parent runner. |
| `iso_root_live_forwarding` | `ISO_EGRESS_ECHO_ADDR`: operator-controlled public numeric TCP echo address, arbitrary port; payload checked across any TCP fragmentation. |
| `iso_root_concurrent_live_tunnels_and_byte_cap` | Same echo fixture; two simultaneous TCP tunnels share the lifetime byte cap; over-budget payload is refused and closes the run. |
| `iso_root_resolved_varlink` | Live resolved Varlink socket/account and `ISO_EGRESS_DNS_NAME`, a public DNS fixture. Run separately for direct and CNAME names on resolved 249/252/255. |
| `iso_root_dedicated_dns_broker` | Installed broker, its fixed upstream TCP53 allow rule, both service accounts and `ISO_EGRESS_DNS_NAME`; child clears supplementary groups then drops to egress uid before using the protected broker socket. Put the compiled test executable under a path traversable by the egress account. |
| `iso_root_installed_daemon_registration_and_revocation` | Installed egress socket/service, root profiles and chosen live DNS route, **no spawnd controller**. First profile: at least 9 total attempts, 32 frames/s, a practical idle timeout (e.g. 30 s). Exercises actual systemd activation, service hardening, DNS startup probe, forbidden IP/translation families, duplicate registration and revocation. The test owns the sole controller and its disconnect ends the daemon; systemd may restart it. |

```sh
# Self-contained root tests, no service installation required:
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --offline -- --ignored iso_root_controller_register_revoke_and_disconnect --test-threads=1
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --test host --offline -- --ignored iso_root_unregister_ack_follows_data_socket_eof --test-threads=1
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --test linux_acceptance --offline -- --ignored iso_root_seccomp_real_syscalls --test-threads=1

# After the installer supplies service accounts, units, root config and DNS:
ISO_EGRESS_ECHO_ADDR=<public-operator-controlled-echo-IP:port> ISO_EGRESS_DNS_NAME=<public-DNS-fixture> CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --offline -- --ignored iso_root_ --test-threads=1
```

The all-tests command requires **both** DNS routes to be installed; otherwise
select the individual applicable test filters. Install/test the unit templates
on Ubuntu 22.04, Ubuntu 24.04 and Debian 12, including private `/run` projections
and real cgroup BPF attachment readback (B1). The root tests have been written
and Linux-cross-compiled, **not executed in this macOS worktree**. Aarch64 Linux
has not been cross-compiled here. Real DNS CNAME behavior, sustained traffic,
runtime proxy compatibility and multi-package lifecycle remain acceptance gates.

### Package C handoff (round 2)

Implementation inside C's scope is present: host-bound public-IP TCP forwarding,
DNS pinning and two DNS routes, bounded CONNECT/data/control handling, aggregate
budgets, audit redaction, service templates/seccomp, environment helper, and the
root controller seam. There is no feature switch enabling this in the gateway.

Remaining integration ownership is explicit:

- A/B1 use `child_environment` with `env_clear`, install/publish trusted config,
  instantiate `EgressController`, and wire lifecycle cancellation and readiness.
- B2 passes the acknowledged endpoint only to the forwarder as fd 3, selects
  the loopback port and root limits, and creates the per-run namespaces.
- B1 installs accounts/units, selects the DNS bind and broker upstream IP rule,
  and reads back unit and cgroup BPF enforcement. C never grants the resolver
  an exception in the egress service itself.
- The coordinator executes the Linux/root matrix, obtains Daybreak review and
  completes multi-package/provider/product acceptance. No deployment, merge,
  release or customer acceptance is claimed by this implementation.

Round 2 changed `controller.rs` (new typed client/seam and lifecycle regressions),
`transport.rs` (bounded fd sending and peer validation), `daemon.rs` (authenticate
its public control entry), `lib.rs` (Linux controller export), `resolver.rs`
(TCP broker regressions), `dns_wire.rs` (alias/NODATA cases), `tests/host.rs`
(fragment-safe live echo), and new `tests/linux_acceptance.rs` (five root tests).
README, this note and the existing Unreleased entry were synchronized. The old
wiki handoff was moved here; no security-gap note remains in `wiki/`.

**CONTRACT CHANGE: none.** No commit was made. The existing worktree/branch belongs
to the coordinating session; this session did not create or remove a worktree.

### Round 1 evidence (historical, moved from wiki)

The first-round handoff recorded 8 portable egress and 29 contract tests passing,
x86_64 Linux and Windows all-target checks passing, and whitespace checks passing.
It listed 20 Linux library tests, 8 ordinary host integration tests and 2 ignored
root tests as compiled only. Those are historical counts, superseded by the
round 2 tests above. Its remaining gates (Linux execution, installed systemd/DNS,
A/B1/B2 wiring, Daybreak and multi-package acceptance) remain explicitly assigned
above; none is inferred from the earlier compile results.

The original package added `Cargo.toml` membership and only a package entry in
`Cargo.lock` (no dependency upgrades), the zh-TW Unreleased entry and docs index;
crate manifest/README; `policy`, `environment`, `http`, `budget`, `transport`,
`session`, `resolver`, `dns_wire`, `forwarder`, `daemon`, `seccomp`, `lib`, `main`;
four systemd templates; and `tests/host.rs`. Public security feature pages were
left unchanged because the assignment limits them to behavior actually enabled.
