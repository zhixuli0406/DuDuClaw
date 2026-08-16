# Resident sensing and signal wake-up

> External data streams wake the AI employee only when a rule actually fires.

---

## Overview

DuDuClaw's Rust gateway already runs 24 hours a day (heartbeat, the autopilot event
bus, CEP, OS perception). This document covers the piece added on top: wiring
**external** data streams — market-quote polling, log files, arbitrary command
output, live WebSocket streams — into that same bus, so cheap deterministic rules
watch them around the clock and only a genuine signal hit wakes the expensive
cloud AI employee.

In one sentence: external data streams enter the autopilot event bus;
deterministic rules (optionally with CEP temporal matching) watch every
observation 24 hours a day; a hit can pass through one more local small-model
screening step; only a signal genuinely worth looking at gets delegated to a
cloud AI employee. The whole path is off by default — with no sources configured,
the system behaves exactly as if the feature did not exist.

---

## Why a wake-up platform needs this layer

DuDuClaw's core interaction model is "an AI employee replies to your message",
but some scenarios need the reverse — the AI employee has to watch an external
number that keeps changing (a stock price, an inventory level, the output of some
program) and speak up on its own only when something abnormal actually appears.
There used to be exactly two ways to do this, and both were bad:

**Option one: let the cloud LLM poll on a timer.** Every poll is an LLM call, and
most of the time the data has not changed or has not crossed any threshold — you
are paying an AI employee to stare at a screen where nothing happens.

**Option two: hardcode a standalone script and run it separately.** The decision
logic lives outside the AI employee's rule system, cannot be protected by the
existing autopilot circuit breaker, and has no unified observability surface —
when it breaks, digging through logs yourself is the only recourse.

Philosophically this maps to the System 1 / System 2 split: Rust rules plus an
optional local small model are the cheap, always-on, never-tired System 1; only
when a rule decides "this deserves attention" does it trigger the expensive,
reasoning-capable cloud System 2. The existing principle of `cep_matcher.rs`
stays intact — temporal matching is always deterministic program logic, never
handed to an LLM to guess.

---

## How it works

### Architecture

```
TickSource (http_poll / command / file_tail / websocket, one tokio task per source)
    │  extract json_fields → derive delta fields → write to ring buffer
    ▼
autopilot event bus (AutopilotEvent::Tick, the same bus as task/channel/cron events)
    │
    ▼
deterministic rule matching (all/any + eq/neq/in/not_in/gt/gte/lt/lte/contains)
    │  CEP temporal rules also work ("price broke the line and no recovery
    │  signal within 60 seconds" — cross-event temporal judgments)
    ▼
circuit breaker (the existing three-state breaker; a misconfigured
    │            high-frequency source cannot blow it up)
    ▼
(optional) local-model screening — asks YES/NO only, never calls the cloud
    │  NO / timeout / unparseable → pass or drop per the on_unavailable policy
    ▼
delegate wakes the cloud AI employee (the prompt can carry the recent observation window)
```

Every stage can be switched off independently: with no sources configured, this
bus stays completely silent; without `screen`, a rule hit wakes the agent
directly (the same behavior rules have for every other event type); when `screen`
answers NO, the wake-up stops right there — `delegate` never even runs.

---

## Configuration: `config.toml [tick]`

The master switch is off by default. One example of each of the four source kinds:

```toml
[tick]
enabled = false                 # master switch, off by default; existing installs are untouched until enabled
allow_command_sources = false   # global gate for command sources, fail-closed
dns_ttl_secs = 60               # seconds a DNS answer that passed the private-network check may be reused; 0 = re-resolve every time

# ── http_poll: GET a URL on a timer ──────────────────────────
[[tick.sources]]
id = "twse-2330"                 # ^[a-z0-9][a-z0-9-]{0,63}$
kind = "http_poll"
enabled = true
interval_secs = 10                # floor 1 second; lower values are raised
url = "https://example.invalid/quote"   # passes the existing SSRF check (rejects localhost / private ranges / cloud metadata)
headers = { "X-API-Key" = "put the key right here" }   # optional, max 8; values never appear in any log or API
json_fields = { price = "/data/price", vol = "/data/volume" }  # field name → JSON pointer
emit_unchanged = false            # no event when the content did not change (default)
max_events_per_minute = 120       # per-source rate cap; excess is dropped and counted
persist_every_n = 0               # 0 = never write to events.db (default); N = keep one audit record every N ticks
baseline_max_age_secs = 3600      # shelf life (seconds) of the change-comparison baseline; 0 = never expires

# ── command: run a command and treat stdout as the payload ──────────
[[tick.sources]]
id = "custom-feed"
kind = "command"
enabled = true
interval_secs = 30
command = ["sh", "-c", "curl -s https://example.invalid/api"]  # argv array, no shell string parsing
json_fields = { level = "/level" }
max_events_per_minute = 60
persist_every_n = 0

# ── file_tail: follow lines appended to a file ─────────────────────────
[[tick.sources]]
id = "trade-log"
kind = "file_tail"
enabled = true
interval_secs = 5
path = "~/logs/trades.jsonl"      # canonicalized on read; the path must actually exist
json_fields = { symbol = "/symbol", qty = "/qty" }
max_events_per_minute = 120
persist_every_n = 0

# ── websocket: hold a connection open; every text message is one observation ────────
[[tick.sources]]
id = "quote-stream"
kind = "websocket"
enabled = true
url = "wss://example.invalid/stream"   # non-local hosts must use wss:// (see below)
interval_secs = 5                      # websocket does not poll; this is the reconnect-backoff starting point, in seconds
subscribe = ['{"op":"subscribe","topic":"quotes"}']  # verbatim messages sent in order after connecting, max 8, ≤4KB each
headers = { "X-API-Key" = "put the key right here" }      # optional, attached to the WebSocket upgrade request
ping_interval_secs = 30                # send a ping after 30 seconds without any inbound message; 0 = off, otherwise min 5
idle_timeout_secs = 300                # recycle and reconnect after 300 seconds with no inbound message, pongs included; 0 = off, otherwise min 30
json_fields = { price = "/data/price" }
max_events_per_minute = 120            # streams are the easiest source to flood; keep this cap in place
persist_every_n = 0
```

Field names in `id` / `json_fields` have reserved words: they cannot be `event` /
`source` / `ts` / `kind`, and cannot start with `prev_` / `delta_` / `pct_`
(those three prefixes belong to the D2 auto-derived fields below). A source that
violates this is disabled at config-load time with a `warn` entry — it never
drags down the whole gateway boot, and every other legal source keeps running.

### Six things to know about the websocket source

The first three source kinds all "go fetch once when the timer fires";
`websocket` instead holds a connection open and waits for the peer to push. Once
inside the system the handling is identical: every **text** message is one
payload, going through the same pipeline of JSON parsing → `json_fields`
extraction → delta derivation → dedup → rate cap → ring buffer + event bus. The
differences are only in how the data arrives, plus these six points:

1. **URL rules are stricter than http_poll's.** Only `ws://` and `wss://` are
   accepted. Any host other than the local machine must use `wss://`; plaintext
   `ws://` is allowed only for `127.0.0.1` / `localhost` / `::1` (the local
   adapter-process scenario). After the scheme check, the host goes through the
   **same** SSRF validation as `http_poll` one more time (private ranges,
   link-local, and cloud metadata hosts are all rejected); a source that fails is
   disabled at config-load time.
2. **`interval_secs` becomes the backoff starting point.** After a disconnect the
   source waits `max(1, interval_secs)` seconds to reconnect, doubling on every
   failure, capped at 60 seconds, with up to 25% random jitter added. A
   connection that survives 60 seconds counts as healthy, and the next
   disconnect's backoff restarts from the starting point. Failing to connect, or
   an error mid-connection, counts toward the `fetch_error` drop counter.
3. **Binary messages are always dropped and counted.** This pipeline eats text
   only. A binary frame records a `non_text` drop (visible on both the dashboard
   and `tick_dropped_total`), and the connection itself carries on as usual. If
   the line never produces data but `non_text` keeps climbing, your source is
   pushing a binary format.
4. **`subscribe` is verbatim — no template substitution.** The messages are sent
   in order after connecting, max 8, 4096 bytes each. If you need to carry a
   token, write it straight into the string (`config.toml` is itself a secrets
   file).
5. **The idle watchdog catches "connected but no data".** A TCP connection can
   stay "open" for hours after the peer stops pushing; the connection state looks
   perfectly normal — only the ticks have stopped. So two clocks run at the same
   time: `ping_interval_secs` (default 30, `0` disables) — after that many
   consecutive seconds without receiving any message, send a WebSocket ping;
   `idle_timeout_secs` (default 300, `0` disables) — after that many seconds with
   no inbound message at all, pongs included, log a `warn`, recycle the
   connection, and reconnect **immediately** (no backoff; only if the reconnect
   itself fails does point 2's backoff apply). Any inbound message (text, binary,
   ping, pong) resets both clocks. When both are enabled, `idle_timeout_secs`
   must be greater than `ping_interval_secs`, otherwise a ping would be judged
   idle before its pong could arrive — a source configured the wrong way round is
   disabled at config-load time. Both also have floors: `ping_interval_secs`
   minimum 5 when non-zero, `idle_timeout_secs` minimum 30 when non-zero. A
   source below the floor is disabled, **not** silently raised — the recycle path
   reconnects immediately (a quiet feed is not a failure and takes no backoff),
   so the timeout value itself is the only upper bound on how often to reconnect
   while the peer stays silent forever; setting it to 1 second builds yourself a
   reconnect fan, and silently bumping it to 30 would only hide your own
   misconfiguration from you.
6. **`headers` attach to the upgrade request.** A feed that needs
   `Authorization` / `X-API-Key` style authentication can carry it right in the
   source config (rules in the next section) — no need to run your own local
   adapter process anymore.

The 64KB per-payload cap and the `max_events_per_minute` rate cap share the same
implementation with the other sources. A stream is the easiest source to shove
tens of thousands of records through in seconds — treat the rate cap as a hard
precondition.

### Custom headers (`http_poll` and `websocket`)

`headers = { "X-API-Key" = "…" }` is attached to every `http_poll` GET and to the
`websocket` upgrade request. `command` and `file_tail` have no request to attach
to; headers written there are stripped at config-load time (so a credential never
gets carried around for no reason). The limits are below; a source that violates
them is disabled outright:

| Item | Limit |
|---|---|
| Count | max 8 |
| Name | `^[A-Za-z0-9-]{1,64}$` |
| Reserved names | `Host` / `Content-Length` / `Connection` / `Upgrade` / `Transfer-Encoding` / `Sec-WebSocket-*` are always rejected (the transport layer generates these itself; overriding them would wreck the connection or forge the handshake) |
| Value | max 1024 bytes, visible ASCII only — CR/LF, control characters, and non-ASCII are all rejected (CR/LF can inject a second header into the request, the classic header injection) |

**Values are always treated as credentials**: they are never written to any log
(not even at debug level), never appear in the `ticks.sources` API (which returns
only a `headers_count` number), and are never carried into the wake-up prompt.
The `warn` message for a rejected config mentions only the header name and never
echoes the value.

Two exception rules: you may set your own `User-Agent` (it overrides the default
`DuDuClaw/1.0`), but `Metadata-Flavor: none` is always forced on — it is a
defense against cloud metadata endpoints, not a convenience option that config
can override.

### Network security: SSRF checks and DNS re-pin

Beyond the SSRF check at config-load time (rejecting `localhost` / private
ranges / link-local / cloud metadata hosts), `http_poll` URLs and non-local
`websocket` hosts **re-resolve DNS at the moment each request goes out or each
connection is made**:

- **Every** IP the host resolves to must be a public address; if even one lands
  in a private range / loopback / link-local, the whole set is rejected (not
  "pick the public one and connect" — a half-blocked check is no check at all).
- Once it passes, the connection is pinned to exactly those just-vetted IPs:
  `http_poll` uses reqwest's address pinning, `websocket` dials TCP straight at
  those IPs, and TLS SNI plus certificate validation still use the original
  hostname (so pinning cannot be used to dodge certificate checks).
- Resolution failure, or a private IP present: **no request is sent**, and one
  `fetch_error` is counted.

What this blocks is DNS rebinding — the attack where the host resolves to a
public IP when the config is written, then points at `169.254.169.254` by the
time the real connection happens. `http_poll` also follows no HTTP redirects at
all (a vetted URL 302-ing into the private network is the classic detour), and
the connection pool is reused on the condition that the resolution result has not
changed, so every round re-validates without rebuilding a TLS connection every
second.

The local plaintext `ws://127.0.0.1` path skips re-pinning: its address is
private to begin with and there is nothing to rebind — a privilege deliberately
kept for the local adapter-process scenario.

**Resolution results are cached for `dns_ttl_secs` seconds (a global setting,
default 60; `0` = re-resolve every time).** A source polling every second that
queried DNS on every round would issue over 80,000 queries a day, for answers
that mostly stay the same for an hour. So each source's task keeps its own map of
"host:port → address set that passed the private-network check + expiry time"
(task-private, no lock needed): within the TTL it reuses the pinned addresses
directly (`http_poll` does not even rebuild the pinned client; a websocket
reconnect skips one resolution wait), and only after expiry does it re-resolve
and re-screen.

This does **not** weaken the rebinding defense — the direction is the opposite:
flipping a resolution answer requires a *fresh* resolution first, and the cache
makes that opportunity rarer, not more common; what is cached is a **set of
already-vetted public addresses**, not a name awaiting vetting. Worst case, the
peer really did move: the connection to the old address fails and the next round
re-resolves. This TTL applies to monitoring sources only; the existing semantics
of `web_fetch` are untouched.

### Extracted numeric strings are converted to numbers

Mainstream market feeds (Kraken, Binance) type their price fields as
**strings**, e.g. `"last": "63669.60000"`. The extraction layer converts a
"clean numeric string" into a JSON number — that is what makes a rule like
`price gt 60000` writable and the change fields below computable.

The conversion is conservative — it would rather not convert than mangle an
identifier:

| Input | Result |
|---|---|
| `"63669.60000"`, `"42"`, `"-3"`, `"0"`, `"0.5"`, `" 7.5 "`, `"1e3"` | Converted to a number (integers stay integers within i64 range, so `eq 42` still holds) |
| `"007"`, `"-007"`, `"00.5"` | **Not converted** — leading zeros usually mean a zero-padded order number |
| `"+5"` | **Not converted** — no feed writes prices that way |
| `"inf"`, `"NaN"`, `"1e400"` | **Not converted** — non-finite values would poison every later comparison |
| `"12 USD"`, `"1,000.5"`, `"0x10"`, `".5"`, `"台積電"` | **Not converted** — the whole string must parse as a number |

Fields that are already numbers, booleans, or null are always kept as-is; this
rule does not touch them.

### A message resolving none of the configured fields is not an observation

When `json_fields` is configured and the payload really is JSON, but not one
configured pointer resolves, the payload is dropped and counted under the new
`no_fields` drop reason: it never enters the event bus, never occupies the ring
buffer, never mixes into the wake-up window. Control messages from real feeds
look exactly like this — Kraken's `{"channel":"heartbeat"}` sits between quotes
and carries no price itself. Early versions emitted it as an observation with
empty fields, and the result was rules being woken repeatedly by content-free
events and the ring buffer filling up with heartbeats.

Two cases are unaffected:

- **A source with no `json_fields`** behaves exactly as before (still emitting
  `raw_len` + a digest of the raw text) — it never worked by field extraction in
  the first place.
- **A payload that is not JSON at all** (say the endpoint returned an HTML error
  page) still emits `raw_len` + a digest of the raw text, and is not counted as
  `no_fields`. That is deliberate: "your feed stopped emitting JSON" must be
  visible as content on the dashboard, not reduced to a counter ticking up.

The drop count shows up as usual in `tick_dropped_total{reason="no_fields"}` and
in the dashboard's drop breakdown. `no_fields` climbing while `events_emitted`
stays flat usually means your pointer paths are wrong, or this source spends most
of its time pushing control messages.

### Auto-derived change fields (no new operators to learn)

For every numeric extracted field, as long as **the value from its last
appearance** is still around, the system automatically adds three fields:
`prev_price` (the previous value), `delta_price` (the difference), and
`pct_price` (the percentage change). A field that has never appeared has nothing
to compare against, so those three fields are simply *absent* rather than `0` —
an absent field satisfies no comparison condition, so it cannot falsely trigger a
rule. When `prev` is `0`, the `pct_` field is also absent (avoiding division by
zero).

**Both computed fields (`delta_` / `pct_`) are rounded to six decimal places;
`prev_` keeps the original value.** Floating-point subtraction leaves noise in
the last few digits: the raw result of `63724.8 - 63724.7` is
`-0.10000000000582077`, and once that value reaches a rule, `delta_price gt 0.1`
gets flipped into a hit by the noise — for a move of barely ten cents. After
rounding it is a clean `-0.1`. Six decimals is a deliberately reserved budget: a
genuinely tiny move like `0.000002` is still fully preserved. `prev_` is the
value the feed itself reported and gets no processing at all. Integer fields keep
an integer `delta_` (`delta_vol eq 200` still holds) — it is never converted to
a float.

**The comparison baseline is per-field, not "the previous tick".** `prev_price`
takes the last time the `price` field actually carried a value; any number of
observations in between without `price` make no difference. On a real market
stream this is decisive: Kraken interleaves `{"channel":"heartbeat"}` between
quotes, and early versions used a whole-record-replacement baseline — one
heartbeat wiped the price baseline, and in live measurement **ninety percent of
ticks could not compute a change**. By the same rule, an observation carrying
only `price` does not touch `vol`'s baseline, and vice versa.

**Baselines expire: `baseline_max_age_secs` (default 3600 seconds, `0` = never
expires).** Per-field baselines have a side effect — a field that stops reporting
for a whole day would, on the next day's first record, compute a giant fictional
move against the day-old value, and a rule cannot tell "a real surge" from "the
line was down for a day". So every field's baseline carries a timestamp, and past
its shelf life the whole thing is discarded: that record's `prev_` / `delta_` /
`pct_` are all absent (exactly the semantics of a first tick), the current value
re-establishes the baseline, and the next record compares normally again. Expiry
is also computed per field — `price`'s baseline expiring does not affect `vol`.

The shelf life has **no floor**, but two configurations log a `warn` at
config-load time (a reminder only — the source still starts):

- **Below 60 seconds**: you get a source that can almost never compute a change.
  Functionally legal (an ultra-high-frequency feed may genuinely only want to
  compare the last few seconds), but because its broken state looks identical to
  "the config is wrong", it is not allowed to happen silently.
- **Shorter than `interval_secs`** (`http_poll` / `command` / `file_tail`): the
  inevitable version of the point above — the baseline this polling round
  establishes has already expired before the next round arrives, so **every**
  tick lacks `prev_` / `delta_` / `pct_`. A once-a-day polling source with the
  default one-hour shelf life steps right into this. The fix is a shelf life
  larger than the polling interval, or `0` to turn expiry off. `websocket` skips
  this check: its `interval_secs` is the reconnect-backoff starting point, not an
  observation interval, so the comparison would be meaningless.

To really turn expiry off, write an explicit `0`.

---

## Autopilot rule example

A rule for "wake the trader agent when the price gains more than 2%":
`conditions` uses the derived `pct_price` field directly, with no string handling
anywhere:

```json
{
  "name": "twse-2330 gain alert",
  "enabled": true,
  "trigger_event": "tick",
  "conditions": {
    "all": [
      { "field": "source", "op": "eq", "value": "twse-2330" },
      { "field": "pct_price", "op": "gt", "value": 2 }
    ]
  },
  "action": {
    "type": "delegate",
    "target_agent": "trader",
    "prompt": "TSMC's stock price gained more than 2% in a short window. Review the recent movement and decide whether action is needed.",
    "context_ticks": 15,
    "screen": {
      "mode": "local",
      "prompt": "Answer YES only if this is genuinely abnormal movement (not normal intraday oscillation)",
      "on_unavailable": "pass",
      "timeout_secs": 10
    }
  }
}
```

- `context_ticks` (optional, default 10, cap 50, 0 disables): when `delegate`
  fires, a compact digest of the source's last N observations is appended to the
  tail of the prompt, so the AI employee sees a trend rather than one isolated
  number.
- `screen` (optional): after the rule hits and the circuit breaker lets it
  through, a local small model is asked one YES/NO question first; only `YES`
  (or a pass under the `on_unavailable` policy when the local model is
  unavailable) actually runs `delegate`. Details in the local screening layer
  section below.
- `screen` is not tick-specific — it is a rule-level general field any
  `trigger_event` can carry; this document just focuses on the tick scenario.

---

## Dashboard observability

System settings → the Autopilot tab gains a read-only card, "Live monitoring
sources" (auto-refreshing every 15 seconds), backed by two admin-only RPCs:

- **`ticks.sources`**: returns every configured source (with `kind` / `enabled`
  / `interval_secs` / `max_events_per_minute`) and its live state
  (`last_tick_ts`, approximate events/minute, cumulative emit count, and
  per-reason counts for the six drop reasons: `rate_cap` / `unchanged` /
  `oversize` / `fetch_error` / `non_text` / `no_fields`), plus the three global
  local-screening counts for pass / drop / undecidable. Custom headers come back
  only as a `headers_count` number — **header values are credentials; no API
  ever emits them**. A source that exists but has never emitted a tick in this
  process returns an all-zero snapshot, not an error.
- **`ticks.recent`**: fetches a single source's recent observations (`source`
  required, `limit` capped at 50), ordered oldest to newest; expanding the card
  shows each record's timestamp and field contents.

The card itself is read-only — adding or enabling a source is an edit to
`config.toml`, not a button on the dashboard.

---

## Prometheus metrics

Served under the existing `/metrics` endpoint:

| Metric | Labels | Description |
|------|------|------|
| `tick_events_total` | `source` | Cumulative count of successfully emitted tick events |
| `tick_dropped_total` | `source`, `reason` | Rejected payloads; `reason` is one of `rate_cap` / `unchanged` / `oversize` / `fetch_error` / `non_text` (websocket binary messages) / `no_fields` (no configured field resolved) |
| `tick_screen_total` | `outcome` | Local screening results; `outcome` is one of `pass` / `drop` / `unavailable` |
| `tick_wakes_total` | `rule` | Times the rule fired on a tick and actually ran its action (past the circuit breaker and screening), labeled by `rule_id` rather than rule name (so user-supplied text never becomes an unescaped Prometheus label) |

---

## The local screening layer

After a rule hits and the circuit breaker allows execution, if `action.screen`
is present, the local inference engine is asked one binary question before
`delegate` / `notify` / `run_skill` actually runs. This layer exists to save
cloud-call money, so it comes with hard limits:

- **Local inference only — it never calls out to the cloud.** This layer never
  touches the account rotator or the cloud API client; if you do not want a
  cloud judgment, simply leave `screen` unset and let a rule hit run `delegate`
  directly.
- **The verdict string is strict.** Only the first whitespace-separated token of
  the reply is examined; after stripping leading and trailing ASCII punctuation
  it must equal (case-insensitively) `yes` or `no`. So `NO.`, `**YES**`, and
  `"yes"` all parse; `I think yes` (the verdict word is not the first token),
  `maybe`, pure punctuation, and `「YES」` wrapped in CJK quotes are all treated
  as "unparseable". This strict rule was fixed during the 2026-08-11 acceptance
  run: early versions could not even parse the period a small model appends, so
  every call fell to fail-open — waking the very cloud call the layer was meant
  to save.
- **Undecidable defaults to pass (fail-open).** If the local engine is not
  attached, the call times out, or the reply cannot be parsed into YES/NO, the
  default is to treat it as a pass — the deterministic condition already hit,
  and screening is only a money-saving second gate. To go conservative instead
  (undecidable means no wake-up), set `on_unavailable` to `"drop"`.
- **timeout_secs** ranges 1–60 seconds, default 10.
- **With an OpenAI-compatible backend, `inference.toml`'s top-level
  `default_model` must be set as well.** Writing `model` inside the
  `[openai_compat]` section alone is not enough — the inference engine reads the
  top-level `default_model`, and without it the call returns `NoModelLoaded`,
  screening is judged "unavailable", and the fail-open default then **passes the
  whole thing through**. The entire episode leaves a single debug log line; from
  the outside it looks exactly like screening passed, when in fact the model was
  never asked. The config looks like this:

  ```toml
  # inference.toml
  default_model = "qwen2.5:7b"     # ← without this line, screening is forever silently unavailable

  [openai_compat]
  base_url = "http://127.0.0.1:11434/v1"
  model = "qwen2.5:7b"
  ```

  To confirm screening is actually running, watch the three screening counts on
  the dashboard's "Live monitoring sources" card: consistently landing in
  "undecidable" is this exact problem.

---

## Operational notes

- **`file_tail` starts from the end of the file (EOF) at boot and never replays
  history.** Deliberate: a gateway restart should not detonate the entire
  existing log as a wave of new ticks. The price is that lines appended while
  the gateway was down are not read back.
- **Tick events do not land in `events.db` by default.** One per second means
  over 80,000 rows a day — too dirty. Recent history lives only in the in-memory
  ring buffer (256 per source) and is cleared on gateway restart. For a source
  that needs an audit trail, set `persist_every_n` yourself (e.g. 10 keeps one
  record every 10 ticks).
- **An idle recycle is not a drop and never appears in the drop counts.** A
  WebSocket idle-timeout recycle logs one `warn` line (with `recycles_total`);
  no new counter is added and `fetch_error` is deliberately not borrowed — a
  quiet feed and a broken endpoint are two different things, and merging them
  into one number only misleads. To tell whether a source is idling, check
  `last_tick_ts` on the dashboard.
- **`command` sources need the global switch.** Writing `kind = "command"` on a
  source alone does nothing — `[tick] allow_command_sources` must be explicitly
  set to `true`. Fail-closed by design: a single source's config is not enough
  to authorize running arbitrary programs.
- **Local screening pointed at a paid endpoint still costs money.** What D4
  guarantees is that "this code path never calls the account rotator or the
  cloud API client" — but the OpenAI-compatible HTTP backend the local inference
  engine supports (`inference.toml [openai_compat] base_url`) can itself be
  pointed by the operator at any compatible endpoint, real paid cloud APIs
  included. Point that `base_url` at a metered service and every screening call
  genuinely spends money; the "zero marginal cost" design premise no longer
  holds. Make sure your local backend really is local or self-hosted.
- **The screening verdict accepts only a strict YES/NO first word** (see the
  previous section). If your operator prompt tends to coax the model into long
  explanations, screening will keep landing in "undecidable" instead of actually
  dropping; write the prompt to demand a one-word answer.
- **The wake-up window is "the last N observations", not a snapshot at trigger
  time.** When one polling round reads multiple lines (`file_tail` picking up
  several lines at once, or `command` emitting multiple records), those lines
  are written to the ring buffer back to back, so the window appended to the
  wake-up prompt may already contain ticks that arrived *after* the triggering
  one. A deliberate semantic choice: an AI employee woken up sees the current
  state, not values already seconds stale. If you need to know exactly which
  value triggered the rule, use rule template fields (e.g. `{price}`,
  `{pct_price}`) — templates always render the fields of the triggering event
  itself.

---

## Not built yet

- Query-signature authentication: `headers` covers `Authorization` /
  `X-API-Key` style header auth, but a feed that requires signing the query
  string with a secret key (with an expiring signature — some exchange
  market-data APIs) still needs a local adapter process of your own, wired in
  via `ws://127.0.0.1:...`.
- A cloud fallback for the screening layer: explicitly ruled out — the layer
  exists to block cloud cost, and adding a cloud escape hatch would contradict
  its own reason to exist.
- Market-data-specific parsers: `command` piping existing tools plus
  `json_fields` extraction covers this today; no exchange-specific built-in
  parsing logic exists.

---

## Related documents

- The autopilot rule engine itself:
  [`23-autopilot-engine.md`](23-autopilot-engine.md)
