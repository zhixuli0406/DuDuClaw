# External systems: query AI employee status

Let a business system (for example an Odoo "my AI assistant" card) read how an AI employee's work is going: how many tasks are in progress, how many wait for you, how many were finished today and in the last 7 days, and the title of the most recently finished one. The external system gets counts and status only — never conversation content or task descriptions.

繁體中文：[外部系統查詢 AI 員工狀態](zh-TW/external-status.md) · 日本語：[外部システムから AI 社員の状況を照会する](ja-JP/external-status.md)

## Which employees are visible

Exactly the AI employees **directly bound** to the account that created the credential, re-read on every request:

- When an admin removes a binding, that employee disappears from the next response.
- A suspended or deleted account's credentials stop working immediately; offboarding an account revokes them permanently.
- Team access a manager gets through reports does not count — only the person's own bindings.
- Archived or deleted AI employees are never listed.

## 1. An admin turns the feature on

The feature is off by default. While off, every request gets the same `404` as a path that does not exist, and no credential can be created. Existing credentials are kept (suspended) and the ones not revoked work again once the feature is turned back on.

**In the dashboard:** an admin opens Settings → Advanced → Security & records, expands "External system queries" and turns the switch on. Read the yellow "Please read before turning on" box first: external systems get only counts and the latest finished item's title (personal data masked); anyone holding a credential can see that; each person may hold 5; turning the switch off suspends every credential. The "Everyone's credentials" table in the same section lists every credential (never the secret) with a Revoke button (confirmation dialog; takes effect immediately and cannot be undone). A credential an admin revoked shows as "Revoked" on that person's External access page.

In `config.toml`:

```toml
[external_status]
enabled = true
```

or, as an admin, call `system.update_config` with `{"external_status_enabled": true}`. No gateway restart is needed: a dashboard change applies to the next request, and a hand edit of `config.toml` is picked up within 2 seconds (the setting is cached for up to 2 seconds).

## 2. Each staff member creates their own read-only credential

Each person creates and revokes credentials for their own account on the dashboard's **External access** page (sidebar → Settings group; employee accounts reach it from the account menu at the bottom left):

- While an admin has not turned the feature on, the page says "An administrator has not turned this feature on yet" and Create credential is disabled.
- The list shows name, created, last used and status (In use / Revoked). A card, "What an external system receives", explains the five values: in progress, awaiting confirmation, done today, done in 7 days and the latest finished item.
- Create credential asks for a name, then shows the secret in a dialog **once**: copy it, then press "I have copied it, close". After the dialog closes the secret can never be shown again.
- At 5 active credentials, Create credential is disabled with a hint. Revoked credentials do not count toward the 5.

The page calls these RPCs, which other clients can use too:

| RPC | Params | What it does |
| --- | --- | --- |
| `me.status_tokens.create` | `{"name": "Odoo card"}` | Create a credential. Name: 1–60 characters. The `secret` in the response (`ddc_status_…`) is **shown this one time only** — paste it into the external system right away. |
| `me.status_tokens.list` | `{}` | Your credentials (name, created, last used, revoked or not). Never includes the credential itself. |
| `me.status_tokens.revoke` | `{"id": "…"}` | Revoke one of your credentials, effective immediately. |

An account may hold at most 5 active credentials; revoked ones do not count. Admins can see everyone's credentials with `status_tokens.admin_list` (never the credential itself) and revoke any of them with `status_tokens.admin_revoke {"id": "…"}`.

RPC error codes (the dashboard shows a friendly zh-TW / English / Japanese message for each):

| Code | Meaning |
| --- | --- |
| `external_status_disabled` | An admin has not turned the feature on. |
| `status_token_invalid_name` | The name must be 1–60 characters. |
| `status_token_limit_reached` | Already 5 active credentials; revoke one first. |
| `status_token_account_inactive` | The account is not active, so credentials cannot be created or used. |
| `status_token_not_found` | No such credential (or it is not yours). |
| `status_token_unavailable` | Temporarily cannot be processed; try again later. |

The server stores only a SHA-256 hash of each credential. A lost credential cannot be recovered: revoke it and create a new one.

## 3. The external system queries

Use the **dashboard gateway** address (the one you log in to, e.g. `https://ai.example.com`; locally `http://127.0.0.1:18789` by default), not the MCP server address.

```bash
curl -s https://ai.example.com/api/v1/status/employees \
  -H "Authorization: Bearer ddc_status_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
```

One employee only:

```bash
curl -s "https://ai.example.com/api/v1/status/employees?agent_id=sales" \
  -H "Authorization: Bearer ddc_status_…"
```

Response:

```json
{
  "generated_at": "2026-11-08T09:30:00.000000000+00:00",
  "employees": [
    {
      "agent_id": "sales",
      "display_name": "Sales Assistant",
      "in_progress": 3,
      "awaiting_confirmation": 1,
      "done_today": 2,
      "done_7d": 9,
      "latest_done": { "title": "Prepare Q4 customer quotes", "completed_at": "2026-11-08T08:12:45+00:00" }
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `in_progress` | Tasks being worked on or queued (to do, queued, in progress, revising). |
| `awaiting_confirmation` | Tasks waiting on a person (review, needs a human decision, pending approval), plus this employee's undecided approval requests that this person could read in the dashboard approval center (Managers and above). |
| `done_today` | Tasks finished today (gateway host time zone). |
| `done_7d` | Tasks finished in the last 7 days. |
| `latest_done` | The most recently finished task this person may see: title and completion time; `null` when there is none. **The title is sent only when the on-device PII detection model is installed** (the "AI smart detection" redaction model — OpenAI Privacy Filter, run locally — downloaded from its card when that rule set is ticked; see [data redaction](../features/55-data-sources.md)) and runs successfully on that title; otherwise `title` is always `null` and only `completed_at` is given. Before detection, invisible, zero-width and bidirectional control characters are removed and the text is NFKC-normalised; a word that mixes look-alike alphabets (for example a Cyrillic `а` inside a Latin e-mail address) makes `title` `null`. The title is then masked before it is cut to 60 characters — credentials, names, addresses, e-mail addresses, phone numbers and account numbers the model finds, plus e-mail addresses, phone numbers (including full-width digits and `+886`), national / resident ID numbers, runs of 8 or more digits (account, customer, card numbers), labelled identifiers (「帳號」「客戶編號」, `account no.`), honorific-marked names (「王先生」, `Mr. Smith`) and names in the shared wiki's people directory, all become `＊＊＊` — and the shortened result is checked again. If the model fails or anything cannot be masked safely, `title` is `null`. The model reduces but does not guarantee the absence of personal data. |

All counts are this person's view, the same as the dashboard: tasks another account started through A2A, tasks whose team hand-off audience does not include this person, and approvals they could not read are left out — two people bound to the same employee may see different numbers.

The response carries no conversation content, task descriptions, task ids or any id other than `agent_id`.

### Error responses

| Status | When |
| --- | --- |
| `404 {"error":"not_found"}` | The feature is off; or `agent_id` is not one of this person's bound employees. |
| `401 {"error":"unauthorized"}` | No credential, a wrong or revoked one, or the account is not active. |
| `400 {"error":"bad_request"}` | A query parameter other than `agent_id`, a repeated `agent_id`, or a malformed one. |
| `405 {"error":"method_not_allowed"}` | Not a `GET`. |
| `429 {"error":"rate_limited"}` | More than 60 calls per minute with one credential, more than 240 requests per minute from one source address (IPv6: per /64), or more than 600 per minute in total. Retry after `Retry-After` seconds. |
| `503 {"error":"unavailable"}` | Task, approval, identity or access data could not be read right now (never reported as zero), or the gateway is already answering 4 status requests and none finished within 2 seconds (`Retry-After: 5`). |

Turning the feature off from the dashboard applies to the next request; a hand edit of `config.toml` applies within 2 seconds.

## Security notes

- This is its own credential type and can call only this one path. Dashboard login, `/api/*` and the MCP server (`/mcp`, `/mcp/v1/call`, `/ingest/transcript`) all refuse anything starting with `ddc_status_`. It is not an `[mcp_keys]` key and MCP scopes do not apply to it.
- No CORS headers: server-to-server only — never embed it in browser code. Responses carry `Cache-Control: no-store`.
- Every attributable call is audited (credential id, account id, result, number of employees) without response content. Calls are aggregated per credential, result and 10-minute window: the first call in a window writes one `status_token.used` / `status_token.refused` audit-log row, and later calls in that window only increase its call count and last-seen time, so an integration polling every second writes at most 6 rows an hour per result. The credential's last-used time is updated at most once per 10-minute window. These audit rows and counts are kept for 90 days, then deleted automatically in small batches; other audit-log entries are not affected.
- Use HTTPS, and revoke a credential as soon as it may have leaked.

## Odoo notes

- The existing DuDuClaw Connector module uses its "Gateway URL" for `/ingest/transcript` (the MCP server) with an MCP API key. Employee status needs the **dashboard gateway URL** and the person's own `ddc_status_` credential; the two are not interchangeable.
- Give each person's card that person's own credential so it shows only their bound employees; do not share one credential across the company.
- The Odoo host must be able to reach the dashboard gateway. The card itself (an Odoo module update) is not part of this release.
