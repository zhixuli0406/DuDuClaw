# TODO — A quota *warning* is counted as a failed call

**Owner:** — &nbsp; **Status:** ✅ fixed 2026-08-17 (server-side; dashboard card still open)
**Last updated:** 2026-08-17 &nbsp; **Severity:** 🟡 MEDIUM (wastes quota retrying calls that already succeeded)

## Fix shipped (2026-08-17)

- **`rate_limit_watch.rs`** (new): both stream parsers (`channel_reply` fresh-spawn
  + PTY diagnostics parser, `claude_runner` dispatcher loop) now parse the
  `rate_limit_event` frame as telemetry — recorded, throttle-logged
  (warn only when utilization moves ≥1 point or the window type changes),
  never a failure.
- **Diagnostics no longer embed the frame**: `last_raw_line` /
  `StreamDiagnostics.last_line` skip `rate_limit_event` lines, so a failure's
  diagnostic string can't smuggle `rateLimitType` ("ratelimittype" ⊃
  "ratelimit") into `is_rate_limit_error` — this was the misclassification
  vector.
- **`is_rate_limit_error` hardened**: advisory-frame tokens are neutralized
  before matching (defence in depth); genuine refusals (`429`, "usage limit",
  "overloaded", …) classify exactly as before. Regression tests use the
  literal frame from this report.
- **Surfaced**: `system.status` now returns `quota_warning`
  (`{status, rate_limit_type, utilization, resets_at, surpassed_threshold,
  observed_at}` or `null`).

**Still open**: a dashboard card rendering `quota_warning` (e.g. "七日配額
92%，8/19 04:00 重置"), and self-throttling of autonomous loops on
`surpassedThreshold` (proposed fix 3 — deliberately not done yet).

## Problem

The `claude` CLI emits a `rate_limit_event` frame on the stream-json channel
as an early-warning signal. It is **not** an error, and it does not stop the
response:

```json
{"type":"rate_limit_event",
 "rate_limit_info":{"status":"allowed_warning","rateLimitType":"seven_day",
                    "utilization":0.92,"resetsAt":1787083200,
                    "isUsingOverage":false,"surpassedThreshold":0.75}}
```

The same run finished normally — `is_error: false`, `result: "PONG"`.

The stream parser drops the frame on the floor:

```rust
_ => {} // system, rate_limit_event, etc.
```

so the warning never reaches the rotator, while some callers still surface the
run as `rate_limit` and rotate away from a perfectly healthy account.

## Why it matters

- **Wasted quota.** A call that succeeded gets retried on another account, or
  the account is put in a cooldown it did not earn — precisely when the
  subscription is already near its ceiling.
- **A useful signal is thrown away.** `utilization` and `resetsAt` are exactly
  what the operator needs *before* running out. The gateway has them in hand
  and discards them.

Observed 2026-08-17 with two experiments sharing one Max 20x subscription: the
seven-day window sat at 92% and dispatches were reported as rate-limit
failures even where the underlying CLI call had returned a result.

## Proposed fix

1. **Parse the frame instead of dropping it.** Treat `allowed_warning` as
   telemetry, never as a failure. Only a real refusal (no result, explicit
   rate-limit error) should mark the account.
2. **Surface it.** Record `utilization` / `resetsAt` / `rateLimitType` so the
   dashboard can show "seven-day quota 92%, resets Aug 19 04:00" — and so an
   autonomous loop can slow itself down rather than discovering the ceiling by
   hitting it.
3. **Consider throttling on the warning.** With `surpassedThreshold: 0.75`
   present, a long-running agent could lengthen its own interval instead of
   burning the remainder at full speed.

## Acceptance

- A run carrying `status: allowed_warning` that returns a result counts as a
  success, and the account is not cooled down.
- Quota utilisation is visible somewhere the operator actually looks.
- A genuine rate-limit refusal still rotates and cools down exactly as today.

## Related code

- `crates/duduclaw-gateway/src/channel_reply.rs` — the `_ => {}` arm that drops
  the frame
- `crates/duduclaw-gateway/src/claude_runner.rs` — `is_rate_limit_error`,
  cooldown handling
