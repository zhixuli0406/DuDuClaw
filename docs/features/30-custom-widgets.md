# Custom Dashboard Widgets

> Your dashboard, your cards — described in plain language or written in raw HTML, always inside a sandbox.

---

## What It Is

DuDuClaw's home dashboard is a list of widgets (agent roster, tasks, channel health, …). Custom Widgets extend that fixed catalog with cards you make yourself, through two authoring paths that share one runtime:

- **Guided AI flow** (every user): pick data sources, pick a presentation style, describe what you want in your own words. The model generates a widget, you preview it live, send feedback for another round, and save when happy.
- **Raw HTML** (admin): a full HTML editing surface with the same live preview — built for distributor engineers customizing a deployment for one enterprise. Widgets export/import as `.json` files, so a distributor can carry a card from one customer install to the next.

Saved widgets live in the **Widget Studio** (`/widgets`): share them instance-wide, add someone else's shared card to your own board, or duplicate it as a starting point.

## The Sandbox

A custom widget is a single self-contained HTML fragment. The dashboard renders it inside an iframe with:

- `sandbox="allow-scripts"` and **no** `allow-same-origin` — the widget gets a unique origin and cannot read the dashboard DOM, cookies, localStorage, or your login token.
- An injected **Content-Security-Policy** that blocks every external resource and network call (`fetch`/XHR included). Data the widget sees cannot leave.
- An injected SDK shim providing the only data door:

```js
const t = await duduclaw.call('tasks.summary');
// { total, by_status, completed_today, recent: [...] }
duduclaw.onTheme((mode) => { /* 'light' | 'dark' */ });
```

`duduclaw.call` proxies a fixed **read-only allowlist** — `agents.summary`, `tasks.summary`, `cost.summary`, `channels.status`, `system.status` — through the *current viewer's* session, so role and data-scope rules apply exactly as they do everywhere else. Anything not on the list is refused, and calls are rate-limited per widget.

Theme follows the dashboard automatically (CSS variables `--fg`, `--muted`, `--accent`, `--card`, `--border` plus a `data-theme` attribute), and the frame auto-sizes to the content.

## Why an Allowlisted Bridge Instead of Trust

Distributor-authored HTML runs on the customer's dashboard. Without isolation, "let the engineer customize the page" means "let anyone who edits a widget act as the logged-in admin". The sandbox flips that: a malicious or buggy widget can — at worst — draw something ugly inside its own card. It cannot escalate, cannot exfiltrate (no network egress), and cannot see more data than the person looking at it is already allowed to see.

## Layouts and Sharing

- A widget joins your board as a `custom:<id>` entry in your personal layout, ordered and hidden like any built-in card. The server refuses layout entries referencing widgets you can't see (fail-closed).
- Sharing is instance-wide and owner-controlled; admins can moderate (remove) any shared widget.
- Managers using the read-only *view-as* mode see a subordinate's custom widgets rendered inline — covered by the same strict-rank grant as the rest of the board.

## Limits

| Aspect | Limit |
|---|---|
| Widget HTML size | 256 KB |
| Bridge calls | 10 per second per widget |
| Bridge methods | 5 read-only summaries (fail-closed) |
| Network egress from a widget | none (CSP) |

Generation runs on the account-rotated Claude CLI path with a Direct-API fallback, with a zero-tool capability set — a widget generation can never touch files or run commands.
