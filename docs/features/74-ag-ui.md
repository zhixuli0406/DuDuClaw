# AG-UI frontend endpoint

External frontends can send a message to an AI employee and receive an AG-UI 1.0 event stream through `POST /api/ag-ui/agents/{agent_id}/runs`. This implements the HTTP/SSE subset described below. It does not implement all 32 event types.

Status: unreleased. The first Daybreak review reported four findings; fixes are in this patch. Gateway compilation, live frontend/provider acceptance and Daybreak re-review remain pending.

## Send a message

Use a dashboard account's access JWT in `Authorization: Bearer …`. Anonymous widgets, widget keys and the legacy shared administrator token cannot authenticate this endpoint. Account status, password-change requirements, credential retirement and the existing MFA policy still apply.

Replace `support` with an employee the account may call. Non-Admin accounts need the existing employee binding or effective team grant and cannot reach the main/default employee through this endpoint. An Admin may select any loaded, operational employee. The JWT and employee access are checked again every 500 ms during the run; expiration or revocation produces `RUN_ERROR` on an active stream.

```bash
# Set DUDUCLAW_ACCESS_TOKEN to the access JWT returned by your normal login.
curl --no-buffer 'http://127.0.0.1:18889/api/ag-ui/agents/support/runs' \
  -H "Authorization: Bearer ${DUDUCLAW_ACCESS_TOKEN}" \
  -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' \
  --data-binary '{
    "threadId": "support-chat-1",
    "runId": "run-1",
    "protocolVersion": "1.0",
    "messages": [{"id":"user-1","role":"user","content":"Please summarize what you can help with."}],
    "tools": [],
    "context": [],
    "forwardedProps": {}
  }'
```

Keep `threadId` for the conversation and use a new `runId` for each request. The gateway derives a conversation key from the authenticated caller, employee and thread; another account cannot select the same history by guessing the thread ID. These sessions use a separate AG-UI segment in the existing WebChat owner namespace; `/ws/chat` refuses to resume them.

Only the latest user message is sent to the existing WebChat reply pipeline. Its content may be a string or an array of text parts. Client-provided system messages, assistant history, `tools`, `context`, `state` and `forwardedProps` do not replace the server's conversation or enter the prompt. The endpoint accepts these transport fields but does not register or execute frontend-defined tools. A matching tool name appears in tool progress only when the employee actually requests a tool already available through its existing runtime; otherwise client tool declarations are ignored. No client state or forwarded properties are echoed in events.

## Read the stream

Each SSE `data:` frame contains one JSON event. A normal reply has this order:

```text
RUN_STARTED (protocolVersion: "1.0")
[TOOL_CALL_START → TOOL_CALL_END, when tool progress is available]
TEXT_MESSAGE_START
TEXT_MESSAGE_CONTENT
TEXT_MESSAGE_END
RUN_FINISHED (outcome.type: "success")
```

The current reply pipeline exposes the checked final text, so text arrives as one content chunk after the existing output and delivery checks. Tool progress carries the observed tool name and a generated call ID. It does not expose raw arguments or tool results; the gateway does not invent `TOOL_CALL_ARGS` or `TOOL_CALL_RESULT` values. Progress availability depends on the employee's runtime.

Invalid JSON, unsupported input or refused authentication/employee access fails before the stream opens. After `RUN_STARTED`, a failure is reported with `RUN_ERROR`. HTTP 200 alone is not proof of success. A connection that ends without a terminal event is truncated. Runtime failures reported by the reply pipeline also become `RUN_ERROR`, rather than a successful text reply.

## Approval and resume

A pending approval belonging to this exact server-owned conversation and turn ends the run with `RUN_FINISHED`, `outcome.type: "interrupt"` and an `interrupts` entry. The entry uses reason `confirmation`, carries the approval ID and expiry, and tells the client whether it can decide through this endpoint (`metadata["duduclaw:canDecide"]`). State and message snapshots precede the interrupt; they contain the endpoint's resume information, not arbitrary client state.

To answer, submit a new run on the same thread with every open interrupt in `resume`. For example, replace `approval-id-from-interrupt` with the returned ID:

```json
{
  "threadId": "support-chat-1",
  "runId": "run-2",
  "protocolVersion": "1.0",
  "messages": [],
  "resume": [{
    "interruptId": "approval-id-from-interrupt",
    "status": "resolved",
    "payload": {"approved": true}
  }]
}
```

Use `{"approved": false}` to deny. `status: "cancelled"` also denies and must omit `payload`. The endpoint does not support `editedArgs`. Missing, extra, repeated, expired or unrelated interrupt IDs are refused; a new ordinary message cannot bypass a pending interrupt. All decisions in one resume are committed in a single transaction. A conflict, changed request, missing origin or incomplete coverage rolls back every decision from that request. Retrying with a new run ID must preserve any decisions already committed by an earlier successful request; entry order and metadata do not change a decision.

Only a current Admin may decide ordinary, unbound `mcp_call` or `mcp_install` approvals here. Dashboard-only records and other approval kinds remain informational. Their decisions must use the original authorized surface. After that surface approves or denies, reconnect with a new run ID and include every interrupt from the last response, including externally decided cards, with the matching approved boolean. The gateway verifies the original request, turn origin and persisted decision before resuming the original pipeline. A caller with current employee access, including a non-Admin, may acknowledge that existing decision without changing it or becoming its decider. Pending cards still require their original authorized surface. Expired, invalidated or mismatched decisions cannot resume. A timely external decision remains confirmable after the card deadline, within the original turn lifetime. The `forbid_self_approval` policy still applies; `metadata["duduclaw:canDecide"]` is display information and does not grant authority. Every submitted decision is authorized again.

The existing turn is retained for a bounded time so an accepted resume can continue its waiting reply pipeline. Approval IDs, expiry and origin correlation are persisted by the existing broker. Runtime continuation is in memory: a gateway restart cannot restore it, and a stale resume returns an error without replaying tools. After restart, a thread with an unfinished turn refuses new work until its original 15-minute lifetime ends. The next new run then withdraws that old turn's pending approvals before admission. An approval decision does not by itself prove that the tool ran or that the provider accepted an action.

Admission is recorded in SQLite. A resume with no live turn and no matching completed receipt is rejected before any journal record is allocated. Per-account quotas persist across restarts and apply across employees and threads, including receipt requests; quota exhaustion returns HTTP 429. Reusing a `runId` in the same thread returns HTTP 409 for 24 hours; this applies even to an otherwise identical request. Use a new run ID when retrying a resume. For up to 15 minutes in the same gateway process, an exactly matching completed resume can return `RUN_STARTED` followed by `RUN_FINISHED` with a success receipt. This receipt acknowledges the recorded decisions: it does not replay the text, rerun tools or prove that the original task succeeded. The receipt cache holds at most 128 entries and is not restored after restart.

## Limits

| Item | Limit or behavior |
|---|---|
| Request body | 256 KiB |
| Input history | At most 256 messages; ID fields at most 128 characters, without control characters |
| Concurrent work | 2 turns per caller, 10 across the gateway, including retained interrupted turns |
| Concurrent streams | A separate 2-per-caller / 10-total limit applies to every SSE response, including errors and receipts |
| Admission journal | At most 4,096 total run/thread rows, at most 256 per account; run IDs retained for 24 hours, duplicate run IDs in a thread return HTTP 409 |
| Admission rate | At most 32 admitted requests per account in a rolling 60-second window; capacity or rate exhaustion returns HTTP 429 |
| Lifetime | 15 minutes for the retained turn, including time waiting for approval |
| SSE keep-alive | Every 15 seconds |
| Slow reader | Bounded event buffering and a 15-second send deadline; a stalled reader cannot retain work indefinitely |
| Disconnect | Active delivery is cancelled and pending approvals for that turn are withdrawn; a completed interrupt intentionally retains the turn for resume |
| Transport | JSON over HTTP/SSE only; no Protobuf or `Last-Event-ID` replay |
| Input/output | Text input and final text reply; no attachment upload or file-delivery API |

Input screening, tool authorization, approval checks, budgets and outbound safeguards remain in the existing reply pipeline. See [security defense](05-security-defense.md). Protocol references: [AG-UI 1.0 HTTP/SSE](https://docs.ag-ui.com/spec/1.0/basic/transports/http-sse.md), [run input](https://docs.ag-ui.com/spec/1.0/basic/run-input.md), [interrupts](https://docs.ag-ui.com/concepts/interrupts).
