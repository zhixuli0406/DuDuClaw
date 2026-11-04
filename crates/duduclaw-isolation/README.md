# duduclaw-isolation

Shared contracts for the planned native Linux employee isolation services. This
crate defines messages, bounds and lifecycle transitions; it does not start a
process, authorize a tool, verify a signature, or provide isolation by itself.
The existing gateway is not connected to these contracts yet.

The normative design is `DESIGN-native-linux-employee-isolation-2026-11`, revision
r7; rustdoc references its sections. Section 17.7 supersedes earlier details.

| Module | Responsibility / implementation package |
| --- | --- |
| `execution` | A: migrate all spawn paths to `ExecutionBackend`; effect registry |
| `ipc`, `ids` | B: spawnd, trampoline, UID registry, account lockdown, admission |
| `egress` | C: host proxy and per-run forwarding; authority stays on the host |
| `capability` | D: gateway MCP, immutable context, verification and revocation |
| `worker` | E: bounded parsing in a network-isolated DynamicUser worker |
| `update` | F: updater, signed manifests, root metadata, durable A/B switching |
| `migration` | G: quarantine, importer, administrator confirmation and readback |
| `execution`, `ids` | H: resource quotas and loop filesystem provisioning |
| `bounded` | Shared size-checked wire values |

## Usage

Use the workspace dependency `duduclaw-isolation = { workspace = true }`.
Only serde, serde_json and thiserror are needed; there are no Linux syscalls or
async runtime dependencies. The contracts compile on non-Linux hosts for tests.

`ExecutionBackend` uses boxed `Send` futures and object-safe I/O traits so all
callers can share `dyn ExecutionBackend` without choosing Tokio or an enum of
backends. Native implementations own OS handles; portable wire handles are
opaque identifiers, never PIDs or raw fd integers.

`SpawnPolicy` is an internal trusted policy, deliberately not serializable.
The spawnd wire request only carries the design's narrow start fields; spawnd
derives argv, environment, cwd and profile from its root-owned registry. Do not
serialize or forward the internal policy across that boundary.

To satisfy the design's capability-before-start ordering, v1 adds a bounded
`ReserveRun(employee)` handshake: spawnd returns an unpredictable handle bound
to that employee and authenticated control connection, valid for 30 seconds.
Reservations count toward the queue/host budget. D creates a pending capability
record for that handle; `StartRun` consumes the reservation once. `TakeIo` then
returns the corresponding gateway socketpair endpoint once, after admission.
D activates the capability only on that connection after checking the handle
and live run; pending records cannot authorize MCP. Cancellation/expiry/disconnect
revokes the pending record and releases its resources. B must reject reused,
expired, wrong-employee and wrong-connection reservations. The backend carries
the caller run for waits so same-employee self-waits return `WouldDeadlock`.

The IPC fd plan covers gateway/spawnd and egress registration. The root-only
trampoline bootstrap must additionally validate r7's executable/mount/statx/seal
matrix and close/reorder descriptors; those Linux operations belong to B.
The migration/update transition helpers check legal ordering only. Implementers
must persist selectors, receipts and readback evidence before advancing state.

Capability signing uses `CapabilityRecord::canonical_bytes()`, independently of
serde/JSON: the domain `duduclaw-mcp-capability` plus NUL, u16 version, fixed
32-byte capability hash, 16-byte employee and run IDs, optional task, principal,
employee binding, lane, tool set, resource set, u64 policy revision, 16-byte
gateway epoch, three u64 Unix-second timestamps, 32-byte nonce and 16-byte jti.
Integers are big-endian. Text has a u32 UTF-8 byte length without normalization;
optional values use 0/1 tags. Sets have u32 counts, sort by encoded bytes and
reject duplicates. Enum tags are fixed in the encoder and covered by golden
tests. D supplies keys, MAC verification, live revocation and resource policy.

EmployeeId is a 128-bit registry key, not a Linux account name. B permanently
maps it to a numeric uid and `duduclaw-emp-<uid in decimal>`; neither identifiers
nor uid/account mappings are recycled. The default uid range is 70000–99999.

Wire v1 uses a four-byte big-endian JSON payload length and exactly one payload
per seqpacket (no trailing bytes). Receive with a fixed maximum-size buffer,
reject truncation and unknown ancillary data, and validate the exact ordered fd
plan before acting. All received fds must be closed on any rejection. Call the
message-specific validators after decoding; schema validation is not peer
authentication or authorization. Implementers must check SO_PEERCRED on the
fixed root-owned socket; PID is never an authority input.

Limits not fixed by r7 are conservative v1 contract choices, not measured
production defaults. Changing schemas, canonical signing bytes or increasing
hard limits requires explicit compatibility review. No backend can fall back
to gateway-uid execution when isolation is unavailable.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
```

## License

Apache-2.0, as declared by the workspace.
