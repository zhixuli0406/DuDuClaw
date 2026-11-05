# Isolated API employee worker (development)

`duduclaw-api-worker` implements the `openai-compat-v1` runtime input protocol.
It is an **employee main run**, not a DynamicUser file parser. The HTTP request,
provider response parser and tool loop execute under B's employee uid, namespaces,
resource limits and egress policy. There is no gateway-uid HTTP fallback.

The fixed root-published argv is:

```text
--duduclaw-protocol openai-compat-v1 --mcp-config /config/mcp.json
```

The gateway supplies a bounded JSON `Request` in sealed fd 4 and writes identical
bytes to stdin followed by EOF. It includes model, optional provider name, system
instructions, history, prompt, output-token limit and deadline. It contains no
API key, endpoint, executable, root, environment override or tool authority. The
worker checks the seal, byte equality, default employee UID range and exact MCP
projection before doing any provider work. It launches only the fixed
`/runtime/duduclaw-mcp-bridge`, inheriting this run's fd 3. Initialization,
`tools/list` and every `tools/call` go through D's bound live capability.

Credentials are independently provisioned **inside this employee's home**, at
`/home/employee/.duduclaw-api/providers.json` (maximum 64 KiB):

```json
{"providers":[{"name":"provider-name","base_url":"https://provider.example/v1","api_key":"employee-owned-key"}]}
```

Do not copy a gateway/shared account key into this file. As with an employee's
CLI login, the employee can read its own key. Names are unique; the requested
provider must match exactly, or an omitted provider requires exactly one entry.
No gateway config or API-key environment variable is read. HTTPS is mandatory,
redirects are disabled, and the run's egress service controls network access.
Credential provisioning/login UI is not added by this package. The release
packager now stages worker/native-adapter ELFs and explicitly supplied signed
runtime policy/vendor artifacts; no production signature or acceptance is generated.

The protocol bounds each provider/MCP response to 4 MiB, provider conversation
to 16 MiB, tool batch to 16 and iterations to 50. A complete batch is validated
before any tool dispatch. Only advertised names and object arguments are sent;
D still authorizes every call independently. Partial output, missing usage,
invalid roles, unknown tools, duplicate call ids, malformed frames, truncated
output and incomplete stdin fail closed. D endpoint closure cancels an in-flight
provider request; final answers require another live MCP request. Runtime and
queue cancellation/retirement stay with A/B.

Gateway consumers refuse unsupported utility, effort, workspace-override,
capability-policy, redaction and CCR modes. The protected SSE entry currently
returns the buffered worker answer. Thinking and native-tool event reporting,
measured usage on failed multi-round runs, non-default uid ranges and provider
compatibility are not closed. A failure after a paid provider call can remain
unmeasured; existing responsibility reservation charging is not provider usage
acceptance.

The root runtime record must pin this ELF, its exact arguments, version,
resources and MCP acceptance, including shim-child fd inheritance and exclusive
config. The new projection digest invalidates older acceptance. No receipt,
profile, readiness flag or installer publication is fabricated here. Actual
signed installation, gateway compilation, ingress wiring and native acceptance
remain necessary. This is not a deployable protected TEAM product.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-api-worker --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-api-worker --target x86_64-unknown-linux-gnu --offline
```

On a Mac without a Linux OpenSSL sysroot, `--no-default-features` additionally
checks Linux Rust code without the native-TLS build dependency. HTTPS then has
no transport and fails; that check does **not** verify the default TLS binary or
network/provider behavior. Build the default feature set on the Linux builder.

## Vendor adapters (round 2)

An employee credential can select `protocol`: `chat_completions` (default),
`responses`, `anthropic`, or `gemini`. Selection belongs to the employee login
file, not the gateway request. All use the same bounded HTTP transport, live D
capability and all-calls validation. Native reasoning/signature blocks are
retained only for replay, never mixed into the answer. Incomplete completions,
unmeasured replies, unsupported block types and invalid tool batches refuse.

| Protocol | Example base URL | Route appended | Authentication header |
| --- | --- | --- | --- |
| `chat_completions` | `https://provider.example/v1` | `/chat/completions` | Bearer |
| `responses` | `https://api.openai.com/v1` | `/responses` | Bearer |
| `anthropic` | `https://api.anthropic.com` | `/v1/messages` | `x-api-key`, fixed API version |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta` | `/models/<validated-model>:generateContent` | `x-goog-api-key` |

These are source adapters with offline fixtures. No provider request was made.

## Native Codex / Grok adapter (round 2)

The package also builds `duduclaw-runtime-adapter`. Root registers it as
`runtime-native-adapter-v1` with exactly `--duduclaw-protocol codex-v1` or
`--duduclaw-protocol grok-v1`. The bounded sealed fd-4 request contains only
`protocol`, `model`, and the fully framed prompt (maximum 64 KiB). Codex additionally
consumes identical stdin through EOF; Grok reads no terminal input. The adapter
execs only `/runtime/runtime-vendor-codex` or `/runtime/runtime-vendor-grok`, inside
the existing employee process; fd 4 is closed on exec and D's fd 3 is inherited.
Codex's inner sandbox is disabled only inside B's protected run. Grok uses its
verified print/permission flags and the A/B PTY API supplied by I1e.

B1 constructs a bounded private home with root-owned native config ancestors
and fixed links into the immutable `/config` projection. Persistent employee data
is reachable at `/home/employee/data`; login references only that employee's
`.codex/auth.json` or `.grok/auth.json`. Named native session/cache directories
are writable and ephemeral. No gateway home, environment or key is copied.
B2 creates `/run/native-project` as a root-owned working directory, links its
`workspace` to `/work`, and for Grok provides both native/project MCP config.
PL-0 refuses an external `CODEX_HOME` (even empty), or any `.codex` entry in the
employee work root, including dangling links. No employee config is merged.

**Acceptance boundary:** no exact vendor CLI was executed. The private config
directory intentionally prevents replacing its root-owned entries; vendor
startup-state writers and atomic login renewal must be exercised with each exact
version. Persistent login refresh is not implemented/verified by this adapter;
do not grant a writable config-directory fallback. Native utility, effort,
workspace overrides, explore, native tool restrictions and capability-policy
modes refuse until their adapters are implemented. Missing actual signed MCP
acceptance still prevents execution. I1e must be merged before protected Grok PTY
can execute; its A/B implementation is not copied into this worktree.

## Signed installation inputs

`package-linux-release.py` builds both worker ELFs. Offline release templates can
include `runtime_entry` install targets in addition to the ten required services:

| Registry ID | Installed generation filename | Mode | Source |
| --- | --- | --- | --- |
| `api-worker-v1` | `runtime-api-worker-v1` | `0755` | built `duduclaw-api-worker` |
| `native-adapter-v1` | `runtime-native-adapter-v1` | `0755` | built `duduclaw-runtime-adapter` |
| `vendor-codex` / `vendor-grok` | `runtime-vendor-codex` / `runtime-vendor-grok` | `0755` | explicit runtime artifact directory |
| `isolation-policy-v1` | `runtime-isolation-policy-v1` | `0444` | explicit reviewed policy, maximum 1 MiB |

Use `--runtime-artifacts-dir` for the last two rows. The policy's native record
must include `vendor_digest` for Codex/Grok; it is part of the launch-policy digest
in `mcp_acceptance`. Changing a vendor therefore invalidates acceptance even when
the adapter stays identical. B1 matches every runtime ELF entry/digest/mode to the
signed generation and requires the exact adapter entry for these protocols.
No new isolation wire type or install-target variant is added. The bootstrap
verifier accepts only complete base generations plus bounded signed runtime
entries with exact modes; staging/publication is not activation or readiness.
