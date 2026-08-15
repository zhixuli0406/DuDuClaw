# Office Document Suite

> Agents that hand you a real .docx, not a paragraph describing one — with a marker protocol, a fail-closed path fence, and a safety net for the day the model forgets the marker.

---

## What It Is

DuDuClaw agents can produce actual office files — Word (`.docx`), Excel (`.xlsx`), PowerPoint (`.pptx`), PDF — and deliver them into the chat channel the request came from, as native attachments. Four bundled document skills teach the agent how to build each format; the gateway owns everything after the file exists: validation, channel delivery, archiving, and dashboard preview.

The suite also works inbound. When a message carries a document attachment, a deterministic extension table (`office_docs.rs`) maps it to the right skill — `doc`/`docx` → docx, `xls`/`csv`/`xlsx` → xlsx, `ppt`/`pptx` → pptx, `pdf` → pdf — and forces that skill into the active set for the progressive skill ranker. Pure lookup, zero cost when no document is attached.

## The 📎DELIVER Protocol

After producing a file, the agent appends one marker line per file to its reply:

```
📎DELIVER:/home/u/.duduclaw/agents/sales/q3-report.docx
```

The gateway then:

1. Strips every marker line from the user-visible text.
2. Validates the path **fail-closed**: it must be absolute, exist as a regular file, and — after canonicalization, which resolves `..` and symlinks — live inside the agent's own directory or the shared `attachments/` fallback. A traversal like `agents/me/../victim/secret` canonicalizes out of the agent root and is rejected.
3. Reads the bytes and sends them through the channel's `send_document`.

A reply with no marker is returned byte-for-byte — the common case stays prompt-cache- and formatting-stable. Any validation, read, or send failure degrades honestly: a text note naming the file's location is appended, so the user is never left wondering where the deliverable went. Silent drops are not an outcome.

## Why a Marker Instead of Guessing

The reply text is the one hand-off point every runtime shares — Claude CLI, Codex, Gemini, direct API. A marker in the reply lets the *model* declare intent while the *gateway* keeps authority over what actually leaves the sandbox. The alternative — the gateway inferring deliverables from prose — is exactly the failure mode the protocol exists to avoid.

The protocol originally lived only in the office skills' SKILL.md files, and a live incident (2026-07-28) showed the gap: an agent produced a real `.docx`, wrote it to `~/Desktop`, and never emitted the marker. The user got prose claiming the file existed; the gateway had nothing to send or archive. Two fixes landed:

- **Always-on system rule** (`deliver_rules` in `channel_reply.rs`): a static, prompt-cache-friendly block in every channel system prompt — files must be saved inside the working directory, each one gets its own `📎DELIVER:` line, and "describing the file in text" does not count as delivery. A sibling always-on block (`pacing_rules`) fixes a related field report: after a heavy task turn, a bare greeting must get a short reply, not a re-run of the previous task.
- **The sweep**, below, for the half the prompt rule can't guarantee.

## The Sweep Safety Net

`sweep_undeclared_deliverables` is the deterministic net for "wrote the file in the workdir but forgot the marker". After a marker-less reply that *talks about* a produced document (keyword gate: extensions, "Word"/"Excel"/"PowerPoint", zh-TW terms like 檔案/簡報/報表), the gateway scans the agent directory and delivers what it finds, exactly as if it had been declared:

| Constraint | Value |
|---|---|
| File types | `docx xlsx pptx pdf csv odt ods odp` only — never `.md`/`.txt`/`.json` (agents write those as internal state) |
| Recency window | mtime within 15 minutes |
| Recursion depth | ≤ 3, skipping `attachments/`, `sessions/`, `logs/`, `memory/`, hidden dirs |
| Per-reply cap | 3 files, newest first |
| Dedup | a file whose sanitized name + size already exists in the archive was delivered by an earlier turn — skipped |

The keyword gate is a delivery heuristic, not a security decision; the fence stays `validate_deliver_path`. Sweep failures are logged and skipped — the net never becomes a new failure mode for the reply itself.

## Archive, Files Page, Preview

Every delivered file is copied into the agent's `attachments/` directory **before** the channel send, so the deliverable stays browsable in the dashboard Files page even if the send fails (files already inside `attachments/` are not duplicated).

The Files page previews office documents in the browser via `GET /api/files/preview`:

- PDF and images stream inline natively.
- `docx/xlsx/pptx/odt/ods/odp/csv` are converted to PDF by LibreOffice (`soffice --headless`) with an mtime-validated cache under `<home>/cache/preview/<agent>/`, an isolated LO profile (parallel conversions against the default profile fight over its lock), and a 60-second timeout.
- LibreOffice missing → an explicit 503 JSON message, never a broken byte stream.
- Same JWT auth and path fences as the download endpoint.

## Provenance (I-2b)

Every file that lands in `attachments/` used to be an anonymous entry in a flat directory listing — a customer's inbound upload and a report the agent produced for a goal task were indistinguishable. A provenance ledger (`artifacts.jsonl`, next to `tool_calls.jsonl` and `task_changes.jsonl`) now records where each file came from, recorded **at the write site**, never guessed after the fact:

| Origin | Meaning |
|---|---|
| `declared` | The agent hand-delivered it via `📎DELIVER:`. |
| `swept` | `sweep_undeclared_deliverables` recovered it — the file exists but the marker was forgotten. |
| `uploaded` | A human sent it in through a channel (inbound attachment). |
| `produced` | Derived read-side from the task's own change ledger — never persisted to the artifacts file itself. |
| `unknown` | The evidence doesn't reach far enough to say. Honest, not a guess. |

Task attribution is two-tier and labelled the same way: `exact` when the row (or the task's own change ledger) names the task directly, `inferred` when only the claim→review time-window convention places it there (the same window `tasks.changes` uses) — the dashboard never presents an inference as a fact. The merge key is the file's real basename, not the CJK-sanitized display name, so two differently-sourced files that happen to sanitize to the same string are never collapsed into one row. A boot-time backfill fills in history for files that predate the ledger, but only as far as existing evidence (declared/swept records, `task_changes.jsonl`) reaches — anything it can't place stays `unknown` rather than assuming direction.

Two surfaces read the ledger:

- **Task detail → 產物 (Artifacts) tab**: every file tied to a task, with its origin, round, and (for `uploaded`/`declared`/`swept` rows with an archived copy) a download link. A file the agent wrote but never archived shows the path it was written to instead of a dead download link.
- **Files page**: a 來源 (origin) column plus a task filter, so "find last week's deliverable" is a filter, not a scroll.

**Known limitation**: the goal-loop dispatch path does not yet archive a downloadable copy of files it produces — those rows can appear as `produced` with no attachment behind them. Closing that gap is tracked for a future wave.

## Limits

| Aspect | Limit |
|---|---|
| Delivered file size | 20 MB (`media::MAX_FILE_SIZE`) |
| Deliver path | absolute, canonicalized, inside agent dir or shared `attachments/` (fail-closed) |
| Sweep | 3 files / reply, 15-min window, depth ≤ 3, office extensions only |
| Preview conversion | LibreOffice required for office types; 60s timeout per conversion |
