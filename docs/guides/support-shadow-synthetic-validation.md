# Synthetic support shadow validation

Run the C7 engineering fixture with one command from the repository root:

```bash
CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway c7_synthetic_shadow_harness --lib -- --nocapture
```

The default test-thread stack is sufficient (verified 2026-09: the harness
completes in ~90s with no `RUST_MIN_STACK` set, which is also how CI runs
`duduclaw-gateway`'s test suite). If a future change to this fixture ever
grows its stack usage past the default, `RUST_MIN_STACK=33554432` raises the
worker thread's stack size — optional, not required today.

The test prints one `C7_SYNTHETIC_SHADOW_SUMMARY=` JSON line. It contains only case counts, review decisions, and failure codes; it does not include ticket IDs, source text, or tenant data. A passing test demonstrates that the current backlog and ticket-SLA shadow engines can process a deterministic 21-day sequence with forecasts committed before each day's outcome source exists. It then checks a missing score, a reviewed correction of the final day's aggregate and ticket outcome, and source removal. Missing, stale, and revoked evidence must suppress complete-window review; saved historical screens retain their original meaning until a linked source is revoked.

Historical dates and ingestion times are staged only in the `#[cfg(test)]` fixture. Production policy, forecast, and score methods still use the wall clock and reject late commitments. This fixture is synthetic engineering evidence. It does not measure skill on real support data, calibrate uncertainty, authorize model promotion, or authorize staffing action. The Decision Lab's synthetic engineering validation remains the dashboard view for a current local fixture; this test's output is deliberately separate from operational evidence.
