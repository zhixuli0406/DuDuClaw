# LINE OA B2C — multiple Official Accounts with credit metering

DuduCloud can host several customers' LINE Official Accounts on one gateway. Each
OA binds to its own agent and carries a credit balance; the customer's end-users
chat with that agent as an AI 客服 and each reply burns credit.

## Configure multiple OAs

`config.toml`:

```toml
[[channels.line.accounts]]
name              = "acme-support"      # label + credit namespace
channel_token_enc = "…"                 # AES-256-GCM (or channel_token plain)
channel_secret_enc = "…"
agent_id          = "acme-agent"
credit_rate       = 2.0                 # points per 1K output tokens; 0 = off

[[channels.line.accounts]]
name              = "beta-shop"
channel_token_enc = "…"
channel_secret_enc = "…"
agent_id          = "beta-agent"
credit_rate       = 1.5
```

The old single-OA layout (top-level `channel_token` / `channel_secret`) still
works — it resolves to one account named `default`. So existing deployments need
no change.

## Credit management (operator)

Points are granted by the operator; billing settlement (topping up with money via
PayUni) is a separate, operator-gated flow.

```bash
duduclaw credit grant acme-support U1234567890 500 --reason "monthly plan"
duduclaw credit balance acme-support U1234567890
duduclaw credit history acme-support U1234567890
```

Metering: each reply costs `ceil(output_tokens / 1000 * credit_rate)` points.
When a user's balance hits zero and metering is on (`credit_rate > 0`), the reply
is refused **before** any LLM call (fail-closed) and the user gets a top-up
notice. A `credit_rate` of 0 disables metering for that OA.

## Status

The config model, credit ledger, and operator CLI are in place and unit-tested.
Wiring the shared `/webhook/line` endpoint to route by the LINE `destination`
field (per-account signature verify, fail-closed on mismatch) and to gate +
deduct on each reply is the remaining integration step; until then a single OA
works through the legacy path.
