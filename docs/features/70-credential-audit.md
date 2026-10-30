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

Passwords and connection strings have **no fingerprint**. They are deduplicated by source reference and kind; their holders come only from routing facts, not password equality. Context-only matches (including arbitrary `api key:` or `token:` values) never receive fingerprints. Only strict provider-key/token or complete PEM structural matches may receive a keyed fingerprint, and only with hardened **AppBound** key protection. Windows and non-hardened macOS **UserStore**, unavailable protection and JWT/bearer-only matches receive none; there is no unkeyed-hash fallback. The upgrade clears fingerprints created before this restriction. Account hints keep only the first character and domain, for example `a***@example.com`.

The ledger accepts at most 5 findings per message, 50 per stable abuse principal per hour and 100,000 credential rows. Authenticated WebChat uses the authenticated user; channels use channel plus conversation; anonymous widgets share one bucket across reconnects and never become durable user holders. Each credential has at most 32 holders and the ledger at most 200,000 holders, enforced in one transaction, including deduplicated credentials. Quota rows expire after one hour. Excess findings produce an aggregated `credential_detection_throttled` event. These limits constrain registration, not the masking of detected values.

System-configured credentials remain in the adjacent inventory, as read-only metadata. Remote MCP, personal Google connections, MCP environment variable names, browser profiles and MCP client identifiers never expose their values.

Last observed use is available only when the gateway tool loop matches an already-held high-entropy fingerprint. Passwords and connection strings have no fingerprint, and external CLI tool calls may not report a trusted usage link; these rows show “Not observed”. An empty timestamp is not proof that a credential was unused.

## Review and export

Filter by type, status or service. An Admin can edit the service and mark a false positive. Marking a false positive annotates the ledger; it does not unmask stored text. **Export this page as CSV** exports the current filtered page (up to 100 entries), records `credential_exported`, and escapes spreadsheet formulas. An audit failure refuses the export.

Admin RPCs are `credentials.audit_list`, `credentials.audit_get`, `credentials.audit_update` and `security.audit_verify`. The audit chain also has a doctor check. Events cover detection, holder addition, service changes, tool observation, status changes and export; they contain neither values nor account plaintext. A successful result means **current segment internally consistent**. Rotation verifies only the current segment, not archives, external copies or tail completeness. Valid legacy rows are allowed only before the first chained row; malformed rows and later unchained rows fail verification.

The employee tool `credential_declare` accepts only `credential_id` and `service`. The service must match `^[a-z0-9][a-z0-9._-]{0,63}$` and pass credential detection. Employee notes are rejected. Employees can declare only credentials they hold through a signed gateway route. Service and note authority are tracked separately: an Admin note-only edit is preserved, and an Admin service cannot be overridden. Declaration arguments, results and denial details are redacted at the audit sink.

The ledger is gateway-owned at `<home>/credentials/ledger.db` (0600 on Unix). The file guard's Read lane blocks `credentials/`; this is a product guard, not OS isolation from a process with unrestricted host access.

Durable LINE ingress stores only masked text in both modes, including postbacks, filenames and extension fields. It never encrypts or retains the original; existing ingress ciphertext markers are removed on upgrade. Delivery tokens and event identity remain available for durable retries with masked text. Before persistence, registration records only metadata, then validated worker routing adds the employee holder; registration remains best-effort, so a timeout or failure can leave a missing registration or holder. Ledger/key-store waits are capped at 250 ms. Timeouts release delivery permits; a separate four-worker cap bounds uninterruptible background work. Saturation and timeouts deliver masked text even in audit mode. Sources use persisted message ids, LINE ingress references or queue/task references.

## Derived-storage boundary

Masking is enforced at each sink, in both modes: pinned instructions and titles, summaries/compression; semantic/episodic memories, triples, key facts, metadata and FTS; automatic, employee and shared Wiki pages (including their index/FTS); skill recordings/cache and extraction audit; global and per-agent RL JSONL; prediction metrics, learned topics/bigrams and evolution context; working-state snapshots/history; delegation queue payloads/results/errors; task/activity/comment/plan/goal/review records; trajectory anomaly, causal evidence and diagnostic logs; SFT/preference training exports and CSV/XLSX exports. Credentials in frozen execution/authority data are refused instead of changing their approved meaning. All eleven channel message logs mask before truncation; formatted file/stderr events and broadcast logs also mask at their sinks.

In audit mode, only conversation messages in `sessions.db` and the current prompt retain detected plaintext; LINE durable delivery is always masked. These are deterministic detection boundaries, not a guarantee of detecting unknown or transformed secrets. Existing derived-store history and external runtime transcripts are not retroactively scrubbed. LINE ingress links contain only metadata, are capped at 100,000 rows and expire after seven days. Security regression tests were added but **not run**; compilation and native acceptance remain unverified.

Wiki writes beyond the 64 KiB credential scan limit are refused rather than storing unscanned text.
