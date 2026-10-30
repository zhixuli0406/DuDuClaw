# Conversation credential audit

Unreleased. **System → Security → Credential audit** lets an Admin inspect a detected credential's type, masked account, service, source, provider, holders, last observed use and status. The ledger and exported CSV contain metadata, never credential values. Detection is deterministic and does not send messages to a model for classification.

## Modes and limits

```toml
[security]
chat_credentials = "audit" # audit (default) or mask
```

In **audit** mode, the original text remains in conversation history and the current prompt. Detected values are masked before derived summaries, wiki distillation, episodic memory, skill recording and diagnostic/tool logs. In **mask** mode, new inbound messages replace detected values with a reference such as `[已遮罩的密碼 cred_xxx]` before delivery. Switching modes does not rewrite old conversation history, backups or transcripts held by external runtimes. Unknown mode values are treated as `mask`.

A bare password with no context is not detected. English and Traditional Chinese context rules and known key/token/PEM/connection-string shapes improve coverage, but detection is not a guarantee that all secrets are found. Scanning is bounded at 64 KiB. Detector failures give derived stores a generic placeholder; a failed ledger write must not block a message or log its value. Automatic substitution of real credential values into tool calls is planned; no release version is promised.

## Identity and deduplication

Passwords and connection strings have **no fingerprint**. They are deduplicated by source reference and kind; their holders come only from routing facts, not password equality. API keys, tokens and PEM material may use a keyed fingerprint through credential protection. When protection is unavailable, no fingerprint is computed; there is no unkeyed-hash fallback. Account hints keep only the first character and domain, for example `a***@example.com`.

The ledger accepts at most 5 findings per message, 50 per sender per hour and 100,000 credential rows. Excess findings produce an aggregated `credential_detection_throttled` event. These limits constrain registration, not the masking of detected values.

System-configured credentials remain in the adjacent inventory, as read-only metadata. Remote MCP, personal Google connections, MCP environment variable names, browser profiles and MCP client identifiers never expose their values.

Last observed use is available only when the gateway tool loop matches an already-held high-entropy fingerprint. Passwords and connection strings have no fingerprint, and external CLI tool calls may not report a trusted usage link; these rows show “Not observed”. An empty timestamp is not proof that a credential was unused.

## Review and export

Filter by type, status or service. An Admin can edit the service and mark a false positive. Marking a false positive annotates the ledger; it does not unmask stored text. **Export this page as CSV** exports the current filtered page (up to 100 entries), records `credential_exported`, and escapes spreadsheet formulas. An audit failure refuses the export.

Admin RPCs are `credentials.audit_list`, `credentials.audit_get`, `credentials.audit_update` and `security.audit_verify`. The audit chain also has a doctor check. Events cover detection, holder addition, service changes, tool observation, status changes and export; they contain neither values nor account plaintext. Chain verification checks the available log, not external copies or a deleted log's completeness.

The employee tool `credential_declare` accepts only `credential_id`, `service` and `note`. Employees can declare only credentials they hold, through a signed gateway route, and cannot override an Admin-set service. Do not put credential values into service names or notes.

The ledger is gateway-owned at `<home>/credentials/ledger.db` (0600 on Unix). The file guard's Read lane blocks `credentials/`; this is a product guard, not OS isolation from a process with unrestricted host access.

Durable LINE ingress masks the stored text and encrypts its original transport copy, bound to the message id; only the delivery worker restores it. If the transport key cannot be used, delivery continues with the masked text and the original cannot be registered downstream. Existing ingress rows are not rewritten. Ledger/key-store work is limited to four concurrent jobs and a 250 ms wait; saturation or timeout delivers masked text even in audit mode. Sources use persisted channel message ids or queue/task references.
