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

**CONTRACT CHANGE (2026-11-05): capability encoding v2.** Resource tag 9 is
`EmployeeScope { id }`, MAC-covered and bound to the record's employee. It means
current live employee authority for Service/Connector calls, not a wildcard
host path. Scope cannot coexist with a task restriction or finite task/table/
wiki/mailbox/recipient/connector grants. Explicit workspace and origin entries
remain required for File/A/E confinement; they do not narrow Service/Connector
employee scope. Resource tag 10, `Confined { resource }`, allows exact File/A/E
input/output grants alongside EmployeeScope (including wiki and connector
outputs). It cannot be nested or wrap Employee/EmployeeScope. These grants do
not authorize ambient Service/Connector access. Both full-record and resource-tag
golden bytes are versioned.
Old v1 capability MACs fail verification; IPC wire v1 is unchanged.

`RequestContext::verify_live` requires the original connection's host verifier;
plain `from_verified_record` contexts are suitable for policy fixtures but
cannot perform I/O. The verifier is in-memory only and is not serialized into
capability bytes.

Protected MCP helper execution additionally requires
`ExecutionBackend::supports_authorized_workspace` and `spawn_authorized`.
The default implementations refuse. The boundary passes an `ExecutionAuthority`
that verifies the actual retained workspace directory and live request before
host handoff; matching a workspace string alone is insufficient. Backends must
not advertise this support before implementing the root identity and final I/O
checks. This is an in-process API extension; IPC wire v1 is unchanged.

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
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
```

## I1e additive control message

`SpawnRequest::PtyActivity { run }` is a new fd-free request with `Ack` response.
Existing v1 message encodings, canonical capability bytes and hard limits are
unchanged. `Resize` additionally accepts an unused reservation to bind initial
terminal dimensions; a subsequent non-PTY StartRun is refused. Queued/Starting
runs cannot change this binding. The root plan carries the validated size to B2
for readback before fork/exec.
Only A reports successful nonempty PTY byte transfers on the original control
connection, after `TakeIo`; B1 supplies the timestamp and refuses terminal,
draining, non-PTY, untaken and already-idle runs. Status/resize are not activity.
A is part of the trusted gateway, so this is not independent kernel byte
telemetry. An older peer rejects the unknown operation; components must be
upgraded together. No compatibility fallback disables the idle timeout.

## Package H work broker ops (H1) and opcode registry

`ipc::Opcode` gives every `SpawnRequest` variant a stable number. The JSON
wire still carries only the `op` name; the number is for audit/telemetry and
collision checks, and `SpawnRequest::opcode` is exhaustive so an unregistered
variant does not compile. Ranges, never reused: `0x0001–0x00FF` core run
lifecycle (`reserve_run` 0x01 … `take_io` 0x08), `0x0100–0x01FF` Package H
(`open_work_file` 0x0101, `work_anchor` 0x0102), `0x0200–0x02FF` HX in-run
helpers, reserved in `HX_RESERVED_OPS` (`helper_call` 0x0201, `helper_wait`
0x0202, `helper_stop` 0x0203, `helper_cancel` 0x0204). HX also owns the result
tags `helper_started`/`helper_ended` and the error codes `helper_busy`,
`helper_unavailable`, `run_time_exhausted`, `helper_cgroup_unavailable`.

- `OpenWorkFile { run, employee_id, path: WorkPath, mode: read_only }`, no fds →
  `WorkFile { run, file: { size, inode, device } }` + exactly one `WorkFileRead`
  fd (regular file, O_RDONLY, FD_CLOEXEC). `WorkPath` is relative to the live
  run's attached `employee-work` mount: non-empty, ≤ 4096 bytes, ≤ 64 components
  of ≤ 255 bytes, no `.`/`..`/empty component, no leading `/`, no `\` and no
  `char::is_control` character (C0 incl. NUL, DEL, C1 incl. U+0085; same rule
  and conformance vectors as mcp-boundary `WorkRelativePath`), first component
  not `.forks`; checked on construction and on decode. `size` ≤ 16 MiB,
  `inode`/`device` non-zero.
- `WorkAnchor { employee_id }` → `WorkAnchor { employee_id, anchor: { device, inode } }`,
  no fds.
- New `ErrorCode`s: `work_file_not_found`, `work_file_refused`,
  `work_file_too_large`, `workspace_broker_unavailable`.
- `check_work_reply(request, response, received_fds)` pairs a reply with its
  request (run/employee) and checks the fd count. Callers still `fstat` the fd.

Protocol stays v1 (additive variants); gateway and spawnd ship together. Until
H2–H4 land, spawnd answers both ops with `workspace_broker_unavailable`.

## License

Apache-2.0, as declared by the workspace.
