# Privacy regression suite (P3-5)

`commercial/docs/TODO-os-native-agent.md` P3-5: "隱私回歸案例納入 duduclaw eval（更新
不得回退隱私性質）". This directory is the **index** for that regression suite —
"an update must not silently regress an OS-native privacy/security property".

## Why these cases are Rust integration tests, not `duduclaw eval` TOML files

[`duduclaw eval`](../../docs/guides/evals.md) regression-tests **agent
behavior**: one prompt through the CLI harness, a stream-json transcript, and
deterministic `[expect]` / `[[expect.grounded]]` assertions over what the agent
said and which tools it called. The four properties this work package covers
are invariants of the **gateway/security in-process pipeline itself**
(a capability gate, a content sanitizer, a fail-closed scorer) — there is no
agent turn to record a transcript of. Hand-authoring a `*.transcript.jsonl`
fixture that merely encodes our own expectation as canned JSON would exercise
**zero** production code; that is an explicitly-rejected "fake eval" per this
work package's own instructions.

Instead, each case below is a standalone Rust integration test that drives the
real `pub` entry point production code (`McpDispatcher::dispatch_tool_call`,
`ProactiveGate::evaluate_with`, `sanitize_perception_text`, `sanitize_osascript`,
`audit::log_injection_detected`) uses — physically **outside** the crate's own
`src/*.rs` (which are read-only for this work package, and some are actively
being edited by other in-flight work packages: `mcp_dispatch.rs`,
`situation_classifier.rs`, `approval.rs`, `channel_reply.rs`, the memory crate).
A future change to the implementation's own co-located `#[cfg(test)]` module
does not weaken this suite — that is the entire point of an "external
yardstick" (see `docs/guides/evals.md` → "GVU integration: the external
yardstick").

## Case index

| # | Property | Defense line | Test file | Cases |
|---|----------|---------------|-----------|-------|
| (a) | `os_native=false` (absent / explicit `false` / malformed config) ⇒ **every** `os_*` MCP tool is denied fail-closed | `duduclaw-cli/src/mcp_dispatch.rs` §3.62 OS-native capability gate (`OS_NATIVE_TOOLS` deny-by-default) | [`crates/duduclaw-cli/tests/privacy_regression_os_native_gate.rs`](../../crates/duduclaw-cli/tests/privacy_regression_os_native_gate.rs) | 4: absent-config deny (all 6 tools), explicit-false deny (all 6 tools), malformed-config deny (all 6 tools), positive control (`os_native=true` clears the gate) |
| (b) | An injected file name / perceived string (`"ignore previous instructions.pdf"`, `<system>…</system>`, ChatML/`[INST]` markers) reaching a delegate prompt is neutralized — structural break-out bytes stripped, flagged `suspicious`, never silently dropped | `duduclaw-security/src/perception.rs` `sanitize_perception_text` + `SanitizedText::as_xml_data` (P2-5) | [`crates/duduclaw-cli/tests/privacy_regression_perception_neutralize.rs`](../../crates/duduclaw-cli/tests/privacy_regression_perception_neutralize.rs) | 4: instruction-override filename, `<system>` tag filename (asserts the wrapped `<perception_data>` fragment a delegate prompt embeds contains no raw `<system>`), ChatML/`[INST]` markers, false-positive guard on normal CJK/accented filenames |
| (c) | `[proactive] enabled=false` ⇒ suppress without ever spending an LLM call; LLM scorer error / unparseable output / out-of-domain score ⇒ suppress (fail-closed, never interrupts on uncertainty) | `duduclaw-gateway/src/proactive_gate.rs` `ProactiveGate::evaluate_with` (P2-2/P2-3) | [`crates/duduclaw-gateway/tests/privacy_regression_proactive_gate.rs`](../../crates/duduclaw-gateway/tests/privacy_regression_proactive_gate.rs) | 6: disabled-agent (scorer closure panics if invoked), LLM error, unparseable output, out-of-domain score, positive control (valid high score allows), adversarial `<system>`-tagged event text does not bypass fail-closed suppression |
| (d) | `os_notify` content carrying an injection payload is neutralized **but not dropped** (non-blocking — "外部內容降格為 DATA", never silently discarded) and the hit is written to the security audit trail; clean content produces no audit noise | `duduclaw-cli/src/mcp_dispatch.rs` §3.63 `neutralize_os_notify_args` + `duduclaw-os/src/notify_native.rs` `sanitize_osascript` + `duduclaw-security/src/audit.rs` `log_injection_detected` | [`crates/duduclaw-cli/tests/privacy_regression_perception_neutralize.rs`](../../crates/duduclaw-cli/tests/privacy_regression_perception_neutralize.rs) | 2: injected title/body neutralized + still non-empty + audited (`blocked:false`), clean content produces zero audit entries |

**Total: 16 cases, 3 files, 3 crates.**

### Why (d) doesn't drive the real OS notification handler end-to-end

`handle_os_notify` shells out to `osascript -e 'display notification …'` on
macOS. Running that in a `cargo test` suite would pop a real, visible desktop
notification on every test run on a developer machine — an environment side
effect this suite deliberately avoids. Instead the (d) tests call the exact
two pure functions `mcp_dispatch.rs` composes before the real send
(`sanitize_perception_text` then `sanitize_osascript`) plus the same
`audit::log_injection_detected` call — genuinely exercising production code,
without the host side effect. See the doc comment at the top of
`privacy_regression_perception_neutralize.rs` for the full reasoning.

## Running the suite

```bash
# Offline/deterministic — no network, no credentials, no live agent.
# Property (a) + (b) + (d):
cargo test -p duduclaw-cli --test privacy_regression_os_native_gate
cargo test -p duduclaw-cli --test privacy_regression_perception_neutralize

# Property (c):
cargo test -p duduclaw-gateway --test privacy_regression_proactive_gate
```

Last verified (2026-07-23): **16/16 passed**, 0 failed, 0 ignored, across the
three files above, in a single fresh run.

## Known trade-offs / scope not covered here

- `OS_NATIVE_TOOL_SURFACE` in the (a) test file is an **independent, hand-kept**
  literal list — deliberately *not* a re-export of `mcp_dispatch.rs`'s own
  `OS_NATIVE_TOOLS` constant (re-exporting it would let the implementation and
  the yardstick drift together). If a future `os_*` tool is added to
  production and this list isn't updated, the suite stays green but silently
  loses coverage for the new tool — a manual step, flagged here so it isn't
  forgotten.
- P3-2 (five-control-point sensitivity labels + context-collapse defense,
  e.g. "screen/clipboard/health data must not be co-mingled with work context
  in one prompt") is a separate, not-yet-implemented work package; no
  regression case exists for it yet because there is no implementation to
  regress against.
- This suite does not attempt to cover the P3-1 VeriOS situation classifier
  (`situation_classifier.rs`) or the P3-3 CEP matcher (`cep_matcher.rs`) — both
  already ship with their own thorough inline unit test suites (18 + N cases
  respectively per `TODO-os-native-agent.md`) and are outside this work
  package's four named properties (a)-(d). A future P3-5 follow-up could add
  an external yardstick for those two the same way this suite does for (a)-(d).
