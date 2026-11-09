# Production Claude ingress acceptance (I1a)

`isolation_product_chain.rs` is a Linux-only, ignored privileged acceptance test named
`iso_root_e2e_claude_production_ingress_product_tool`. It is a real-provider test;
provider calls may incur cost. It does not install a fake authority, replace a
producer, manufacture a JWT, call a product handler directly, or inject a turn
context. A missing prerequisite is an assertion failure, never an early success.

## Required disposable environment

Use a dedicated disposable Linux VM, never an operator's active installation.
Provision it through the actual signed installer/update/migration flow before
running the test. Independently verify the release signature and pinned generation,
including gateway, spawnd, trampoline, egress, bridge, worker and updater. The
SHA-256 values below identify that already verified generation; this fixture
itself is not a signature verifier or installer test.

The VM must have systemd and cgroup v2, root `/proc` visibility, and the actual
protected TEAM gateway unit `duduclaw-gateway.service`. Its data home must be
`/var/lib/duduclaw/.duduclaw`. Keep the gateway stopped (`inactive`, MainPID 0),
its configured port unused, and background schedules/dispatch disabled for the
acceptance employee. Other isolation services must already be correctly
provisioned and available. Set `[gateway] bind = "127.0.0.1"`; neither the unit
nor an environment file may override this with a public bind.

Provision a **test-only** unit drop-in with `Restart=no` before invocation. The
fixture requires this setting and never edits unit files or executes
`daemon-reload`. It starts only an inactive gateway, records its systemd
InvocationID, and stops only that same invocation on normal completion or Rust
panic. It never restarts or stops a previously running gateway, nor kills by UID.
The guard is armed before start; after a start timeout or failed first readback
it attempts to recover the new invocation from the previously inactive unit on
this exclusive VM, then confirms that ID again before stopping. An unreadable
InvocationID, process abort/SIGKILL, or concurrent lifecycle change needs operator
inspection/disposal; no test can promise RAII cleanup after process termination.
Keep this VM exclusive while the test runs. The stopped service is the original
state to which normal cleanup returns; the fixture does not delete data.

Pre-provision an actual dashboard account with password login, no pending
password change, and access to one existing Claude employee. Optional MFA uses
one real recovery code, which is consumed by a successful login. The account
must be allowed `system.status` and WebChat. Its live ID and credential epoch
must come from actual account provisioning, not test-written user records.
The employee must use the real native Claude runtime and permit `tasks_create`
without a manual approval interruption. Keep the employee idle, with no
preexisting process under its isolated UID (at least 70000), and do not use the
same UID for other services. Tasks, sessions and run-step product databases must
already exist with current migrations.

Provision provider login/credentials **only in the employee's isolated home**
using the approved provisioning mechanism. The account must be able to reach
its provider through the real egress service. Do not copy the gateway's HOME,
environment, API key, credentials, token, or `.mcp.json` into the run. The
fixture passes no provider credential to systemctl, the gateway, Claude, or the
bridge. It uses the installed gateway's real factories and requires all
composite readiness components. With the currently absent production factories,
an otherwise provisioned VM must fail at the readiness assertion; marking those
components Ready in a test is not an acceptable workaround.

## Private input

Create a JSON file in a root-owned private directory, for example
`/root/iso-acceptance/setup.json`. Every ancestor must be root-owned, not a
symlink and not writable by group/other. The file must be regular, root-owned,
mode 0600, have one hard link and be at most 16 KiB. Avoid putting it under
world-writable `/tmp`. Supply these fields (the values below are placeholders,
not runnable credentials):

```json
{
  "port": 18889,
  "gateway_uid": 999,
  "employee_uid": 70001,
  "agent": "acceptance-agent",
  "user_id": "actual-provisioned-account-id",
  "gateway_sha256": "64-lowercase-hex-from-verified-release",
  "claude_sha256": "64-lowercase-hex-of-real-native-Claude-executable",
  "bridge_sha256": "64-lowercase-hex-of-real-installed-MCP-bridge",
  "email": "actual-disposable-account-login",
  "password": "actual-disposable-account-password",
  "mfa_recovery_code": null
}
```

Populate secrets without shell arguments, command substitution, command history,
CI artifacts, or logs. Do not print this file. The fixture disables its core dumps,
uses non-Debug input and zeroizes credential strings/buffers; actual login tokens
stay inside the existing TUI auth/client APIs. It suppresses client tracing and
does not print server response bodies or process environments.

## Build and invocation

Build on a Linux builder with adequate resources; **do not run this CLI/gateway
build on the shared development Mac**:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-cli \
  --test isolation_product_chain --no-run
```

Transfer the matching test executable to the dedicated VM and verify its
provenance. As root, invoke the actual executable printed by Cargo (replace the
example path; do not pass credentials in argv):

```sh
env -i PATH=/usr/bin:/bin \
  DUDUCLAW_ISO_DISPOSABLE=1 \
  ISO_PRODUCT_SETUP_FILE=/root/iso-acceptance/setup.json \
  /root/iso-acceptance/isolation_product_chain-TEST_HASH \
  --ignored --exact iso_root_e2e_claude_production_ingress_product_tool \
  --test-threads=1 --nocapture
```

A macOS/Windows run discovers zero tests due to the Linux cfg; that is not an
acceptance pass. A normal test invocation reports this test ignored; that is
also not acceptance. Require exactly one executed, passing test on the VM.

## Evidence and limits

The fixture logs in through the original TUI REST account-login/MFA API, opens
the real authenticated RPC/WebChat sockets, and submits a unique task request.
It observes signed-release executable digests for real Claude and the bridge,
all four actual employee UIDs, separate mount/PID/network/IPC/UTS namespaces,
the same spawnd run cgroup, and the runtime's MCP socket on fd 3. It checks that
legacy gateway bearer and user/turn authority environment variables are absent.
Sampling may miss a very short-lived bridge; that yields failure, not inferred
success. The fixture does not directly decode or forge the opaque MCP context.

It reads the actual `tasks.db` row back: one exact nonce/title/description,
actual employee assignee and creator, `created_via=agent`, and
`on_behalf_of=user:<original logged-in account>`. It verifies the product-minted
UUID appears in the actual assistant response, the exact triggering user
message belongs to a `webchat_user` session owned by the original account, and
the same session's original CLI stream records `mcp__duduclaw__tasks_create`.
The test never creates that task or edits these databases itself. These checks
cover a successful human ingress/product tool chain; they do not replace
negative stale-epoch, cross-user, replay, cancellation or provider-failure tests.

Product task/session/run-step rows remain as disposable acceptance evidence.
Remove the private input, including any remaining recovery code, and dispose of
the VM after recording a redacted result. Do not publish DBs, provider state or
logs containing conversation/account secrets.

At authoring time this fixture has only passed a standalone Rust syntax parse;
CLI/gateway compilation and privileged/provider execution have **not** been run.
No live acceptance result is claimed.
