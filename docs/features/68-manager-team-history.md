# Read-only team conversations

Unreleased Enterprise dashboard and read-only API.

In the Enterprise edition, an active Manager or Admin can read conversations for an AI employee directly bound at Operator or Owner to an active person below them in the reporting tree (up to eight levels). Admin alone grants no access to this view. A forced password change, reassignment, suspension or removed binding takes effect on the next request; permissions are checked again before returning content. Ordinary history, export, download and WebChat resume permissions do not change.

| Conversation | Returned content |
|---|---|
| Your own signed-in WebChat, or an in-scope report's signed-in WebChat | User and assistant text, read-only; attachment basenames and types |
| Messaging app, website widget visitor, Odoo Live Chat | Both sides masked; attachments show type only |
| Another person's WebChat, unknown/unstamped origin, internal task or schedule | Hidden, with no hidden count |

Only visible, non-undone user and assistant messages from the current activation window are returned. Stored titles, summaries, prior context, hidden turns, thinking and tool steps are excluded. List titles come from the first visible user message in that window. Attachment paths and download links are never returned.

## Using the dashboard

Open **My team → Team conversations**, or choose **View conversations** beside an eligible report’s AI staff member. Select an AI staff member, optionally filter by report or conversation type, and open a conversation. External conversations carry a **Masked** label. The view has no composer, resume, export or attachment download action; message text is rendered as text, without loading links or embedded media. The missing-AI-detection warning remains visible when applicable.

Each conversation open is recorded. Changing filters, refreshing or receiving a refusal clears displayed content; returning focus refreshes the reporting scope. A failed request shows an error instead of keeping an earlier transcript. An opaque load-more cursor is passed back unchanged.

Administrators find **Organisation settings → Manager conversation access** on **User Management**. Inform staff before enabling it; turning it off and on starts a new time boundary. The adjacent AI-detection shortcut opens the existing redaction settings. A notice below the WebChat composer and an eye on the staff selector appear only when the gateway says that this person’s conversation with that AI staff member can be read by a manager.

The separate “who viewed my conversations” history is not included in this change.

## The switch and time boundary

```toml
[people]
manager_team_history = true
```

The Enterprise default is on in a valid configuration. Missing/unreadable/invalid configuration or a non-boolean value means off. Personal edition always means off. The gateway records the boundary in `users.db` before channels start, checks it on reads and settings changes, and synchronizes edition changes every 60 seconds. Turning it off and back on starts a fresh window; previous windows remain excluded. A failed disable audit retains a durable reset requirement, so recovering the audit writer cannot revive the earlier window.

Admin RPCs `users.org_settings.get` and `users.org_settings.update {manager_team_history: boolean}` read/change the switch through the locked config writer. A raw config edit is also observed. Direct file edits are seen at the next read or watcher tick: an off/on cycle entirely between observations cannot be detected. Changes do not delete any stored conversations.

## Masking and its limits

External conversations always use the union of `taiwan_strict`, `financial`, `general` and configured custom rules. The model-redaction master/source switches cannot disable these view protections. Existing redaction tokens become placeholders; no vault or reversible copy is created. Sensitive values receive per-conversation category/number pairs, including phone values normalized across punctuation. External sender IDs become `客戶 n`; a verified, active in-scope report may be shown by display name.

Every raw rule match contributes coverage, including low-priority overlaps; priority only selects the placeholder label. External text is normalized with NFKC before scanning and returned in that normalized form. An attachment-bearing line is withheld as one attachment, including ordinary text on the same line, so nested or escaped destination parentheses cannot leave a filename/path suffix visible. Malformed operator-confirmation metadata is withheld through the end of the message.

Rules are also matched against the original text, so normalization cannot bypass a full-width keyword or alter a regex boundary to uncover a value. Original hits map conservatively over complete graphemes; the mapping must reproduce the whole normalized string or the segment is fully masked. Numbering uses fixed-size, freshly salted in-memory digests of whole normalized matches. Split overlaps cannot multiply copies of a large sensitive value; the digests never leave memory, and refreshes keep the same visible numbers.

An absent local NER model is allowed with `ner: false`: **names and addresses may remain visible with regex alone**. A partial installation, rule error, installed-model inference failure or timeout refuses the entire external transcript with `masking_failed`. Installed NER is called through an error-returning API. One blocking masking worker holds its permit even after timeout. No masked transcript is cached to disk or stored in the vault.

This protects the product read path. A person with direct access to the gateway's OS files/databases can bypass these product controls. Regex and NER do not guarantee detection of every personal fact in free text.


A macOS arm64 native check with pinned model revision `7ffa9a043d54d1be65afb281eddf0ffbe629385b` and ONNX Runtime 1.24.2 returned `ner:true` for two synthetic Chinese/English examples (names, addresses, phones and emails). This is a smoke check, not a detection-accuracy benchmark. NER may mask ordinary prefixes too; overlapping detections can produce adjacent placeholders, and counts describe rendered parts rather than distinct people.

## RPC contract

| Method | Parameters / result |
|---|---|
| `users.team_history.list` | Required `agent_id`; optional `report_user_id`, `class` (`own`, `report_webchat`, `external`), `limit` (default 50, max 200), `before`. Returns `sessions`, `next_before`, `since`, `ner`, `read_only: true`. |
| `users.team_history.get` | Required `session_id`; `limit` defaults to 200, max 500. Returns `class`, `since`, `ner`, `read_only: true`, `messages` and `masked_counts`. |
| `agents.callable` | Adds `manager_may_read` when the caller directly holds Operator/Owner and has an active Manager ancestor under the enabled Enterprise setting. |
| `audit.resolve_session_digest` | Admin only; `{digest}` resolves a 16-character per-home HMAC to the local session ID. |

Message parts are `{t:"text",v}`, `{t:"mask",cat,n}` or `{t:"attachment",kind,name?}`. `name` is absent externally. Clients should display localized category placeholders and the regex-only warning when `class` is external and `ner` is false. Hidden and missing sessions share `session not found`.

Pass `next_before` back unchanged; a RFC3339 time is also accepted for the initial `before`. SQL applies scope before pagination, with a compound activity-time/session-ID cursor. A page scans at most 10,000 authorized candidates; a transcript is capped at 20,000 rows/8 MiB. Oversized windows fail rather than silently skipping rows. Mask numbering is calculated over the whole visible window before the newest requested messages are selected. Display text is truncated after masking at 20,000 characters per message.

Retained title inputs across a page are capped at 8 MiB as well; reduce `limit` if the page is too large. Optional filters must be strings (or omitted/null); cursors are capped at 4 KiB and report IDs at 128 bytes. Sender labels use the AI employee's display name when available.

Every successful get requires a durable chained `team_history_viewed` event, without deduplication. Successful lists and refusals are deduplicated per viewer/agent/filter or reason per minute. Audit details contain a per-home session HMAC and counts, never transcript text or raw external session IDs. Audit failure returns `audit_unavailable` without content. Get is limited to 20/minute and 120/hour; list to 300/hour, per viewer and gateway process. A refusal includes `rate_limited` and retry seconds. Reassignment during masking causes a fresh refusal before disclosure.

Counts describe the returned parts after message selection/truncation; numbering still comes from the whole visible window. A boot-time audit error fences team history without stopping the gateway. Recovery must record a fresh window; if the database cannot persist its reset marker, a process latch keeps the view closed until that repair succeeds. Masking admission is bounded to eight requests and a three-second wait, with one actual worker.

See [Calling AI staff](62-calling-ai-staff.md) and [Security defense](05-security-defense.md).
