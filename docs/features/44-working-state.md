# Working state

> One authoritative record of each AI employee's current rules and commitments, valid across wake-ups — injected automatically every time, changed only through audited tool calls.

---

## The problem: three stop-loss lines in one day

A resident AI employee is more than "a conversation". Scheduled patrols, heartbeats, the goal loop, messages from nine channels — every wake-up is a brand-new invocation, and none of them share conversation history. That creates a real, incident-grade failure mode: **an operating rule the employee set for itself may be forgotten by the next wake-up, or replaced by a different version read out of a different note.**

On day three of the autonomous investing experiment, the trading employee wrote down three mutually contradictory stop-loss lines within a single day:

- 09:04 pre-market strategy: 262 ("exit without hesitation if broken")
- A dozen intraday patrols: 254 ("-5% per the strategy document")
- Post-close review: 257 ("admit the mistake if broken tomorrow")

The day's low was 261 — by the first rule the position should have been closed, yet later wake-ups did not even remember that line existed. The root cause was not "lax discipline" in the model; it was an architectural defect: a rule existed only in whichever note the current wake-up happened to read, and notes (journals, strategy documents) are running records by nature — N contradictory entries about the same thing can coexist. The research literature calls this condition **ghost memory**: stale and current facts living side by side and blending at retrieval time.

---

## How it works

Each AI employee keeps one "working state": key-value current rules and commitments (for example `stop_loss.2317 = 262`) plus one handoff note. On **every wake-up**, the gateway injects it at the tail of the prompt automatically, marked as the only authoritative source of current values. To change any entry, the employee must call a dedicated tool with a reason attached; the old value enters an auditable supersession chain. An employee that never set any state gets zero injection and pays zero cost.

### What gets injected

Every wake-up, the tail of the employee's prompt gains this section (automatic — the employee reads no files):

```
## 工作狀態（唯一權威 · 由你自己以 working_state_set 維護）
以下鍵值是你跨喚醒持續有效的工作狀態與承諾，為唯一權威的「現行值」；
筆記、日誌、策略文件裡與此矛盾的數字一律是歷史值（已作廢），不得採用。……
- stop_loss.2317 = 262（08-13 09:04 設；理由：跌破即出場不猶豫；有效至 08-13 13:30）
- position_cap = 96%（08-12 08:42 設；理由：留最低手續費緩衝）
交接註記（08-13 09:36 留）：盤中每 3 分巡檢中；帳務已核對。
```

The section is rendered in zh-TW, the platform's operating language. It declares that the key-value entries below are the employee's authoritative current state and commitments, and that any conflicting number found in notes, journals, or strategy documents is a superseded historical value that must not be used. Each entry carries its value, when it was set, the reason, and (if set) its expiry; the last line is the handoff note.

### The tools (MCP, shared across all five runtimes)

| Tool | Purpose |
|------|---------|
| `working_state_set` | Set or update one key (`reason` required; optional `ttl_hours` lets a day-scoped rule expire on its own; optional `expected_value` for concurrency protection — a mismatch with the current value refuses the write and reports the current value) |
| `working_state_clear` | Retire one key (`reason` required) |
| `working_state_handoff` | Overwrite the handoff note — what the next wake-up's self needs to know (plain text, or see "Structured handoff" below) |
| `working_state_get` | Read everything: all keys (expired ones included, marked as such), the handoff note, and recent supersession history |

### Design points

- **Explicit tool calls only.** The gateway never scrapes "rule-looking sentences" out of the employee's reply text (self-reports are untrusted). Every change lands in the audit log and the supersession chain, and the act of changing a stop-loss line itself shows up in the next round's "recent own actions" section.
- **Concurrency protection (CAS).** Two wake-ups running at the same time (say, a patrol scheduled every 3 minutes) must not overwrite each other — pass `expected_value`, and if another wake-up already changed the key, the write is refused.
- **TTL.** Day-scoped rules like an intraday stop-loss line get `ttl_hours`; tomorrow the entry is no longer authoritative (it stays readable in the file but is no longer injected).
- **A cap that forces convergence.** At most 32 keys; at the cap, adding a new one is refused and the existing keys are listed — a state table that balloons into a second mountain of notes defeats the point.
- **Cost.** The injection sits in the dynamic tail after the prompt-cache layers, so it never breaks the cached prefix; the section is capped at 3KB; an empty state means zero injection.

---

## Structured handoff (Ralph-style, optional)

A plain-text handoff (passing only `note`) is unaffected: whitespace is silently collapsed and content past roughly 1200 characters is silently truncated, as before.

For a stricter handoff, `working_state_handoff` accepts four extra fields — `status` (`continue` / `complete` / `blocked`) plus `next_steps` / `evidence` / `blocker`. Passing any one of them makes `status` mandatory, and the call is validated against `status`; a violation rejects the whole call:

| status | Requirement |
|--------|-------------|
| `continue` | `next_steps` required, non-empty; `blocker` not allowed |
| `complete` | `evidence` required, non-empty; neither `blocker` nor `next_steps` allowed |
| `blocked` | `blocker` required, non-empty |

The reasoning: "done" must never be self-declared — no concrete evidence, no `complete`. And an in-progress handoff without a next step leaves the next wake-up's self a blank slate.

**Oversized handoffs are rejected whole, never truncated**: if the combined byte count of `note` + `next_steps` + `evidence` + `blocker` (CJK-safe counting) exceeds `config.toml [memory] working_state_handoff_max_bytes` (default 16384), the call returns an error and nothing is written. This deliberately differs from the plain-text mode's silent truncation — a truncated structured handoff might lose exactly the evidence or next step that made it trustworthy, while still looking like a complete, credible handoff. That is more dangerous than an outright rejection.

With a `status` present, the block injected into the next wake-up carries the extra fields too:

```
交接註記（08-13 09:36 留）：盤中每 3 分巡檢中；帳務已核對。（狀態=continue；下一步：核對完後回報總額；證據：帳務表已比對三次）
```

The cap is adjustable — see `working_state_handoff_max_bytes` under "Configuration" below.

---

## Division of labor with other mechanisms

| Mechanism | The question it answers |
|-----------|-------------------------|
| Working state (this feature) | What rules am I committed to **right now**? |
| Recent own actions (audit feed) | What have I **done** (including blocked calls)? |
| Goal task `<state>` block | Where does **this task** stand? |
| Memory / learned rules | What have I **learned**? |
| Shared wiki | The team's shared SOPs and reference documents |

---

## Configuration

`config.toml`:

```toml
[memory]
working_state_enabled = true             # default on; gates injection only — tools and files are unaffected
working_state_handoff_max_bytes = 16384  # total byte cap for structured handoffs (see above), CJK-safe counting; plain-text handoffs are exempt
```

The state file lives at `<agent directory>/state/working_state.json`; the supersession history sits in the same directory as `working_state_history.jsonl`. Both are human-readable files — when something looks wrong, open them directly.

---

## Usage conventions for employees (worth adding to the employee's CLAUDE.md)

- The moment a decision parameter is set (stop-loss, take-profit, position cap, current phase), call `working_state_set` **immediately**; give day-scoped rules a `ttl_hours`.
- During patrols, the injected working-state section is the authority — **re-deriving a new value on the spot is forbidden**.
- Before wrapping up any run, leave a handoff via `working_state_handoff` — the context can end at any moment, and a decision never written back might as well never have been made.

---

## The takeaway

A resident employee needs one place where "the current rule" lives, and that place cannot be prose. Working state gives each employee a single authoritative table: injected into every wake-up, changed only through audited tool calls, with old values retiring into a supersession chain instead of lingering as rivals. Notes stay useful as history — they just stop competing for authority.
