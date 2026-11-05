# Office Document Suite

The four office skills are installed once in the company-wide `<home>/skills/` layer, rather than copied into each new employee. Existing copies are preserved. See [Skill Lifecycle](15-skill-lifecycle.md) for optional programming skills and cleanup guidance.

> Agents that hand you a real .docx, not a paragraph describing one — with a marker protocol, a fail-closed path fence, and a safety net for the day the model forgets the marker.

---

## Office script trust boundary

`office_script` executes only the Python scripts embedded in the running binary. Each script must match the compile-time SHA-256 manifest; the binary version and script digest identify its version. Personal, department and company skills can override **SKILL.md prompt text only**. Their scripts are never executed, even when their filenames match a built-in script.

The gateway/MCP process creates a private, uid-owned `0700` directory under `<home>/.office-execution/` for each invocation and writes verified code with exclusive, no-follow creation and mode `0500`. Input files are opened by walking each component without following links, checked for canonical containment, and copied into that private directory. The existing document resource checks inspect this same snapshot. Python runs there with a cleared environment, no employee project/config discovery, and no employee-controlled import directory.

Arguments follow a schema for each script: one input; `create` requires `--out`; `to_pdf` optionally accepts `--outdir`; `--format` accepts only that script's documented values. Use separate tokens (`"--out", "report.pdf"`); `--out=/path`, unknown/duplicate flags and extra inputs are refused. Inputs may be inside the caller's directory or `<home>/attachments/`. Outputs must be **new files inside the caller's directory**, with the matching document extension and an existing parent directory. Existing files, protected structure filenames, symlinks, hard links and traversal are refused. Conversion without `--outdir` uses the original input's parent, which must also be inside the caller's directory. Python writes only into staging; Rust rewalks the destination and publishes after success: the complete file is synced in private staging, then moved with an exclusive, no-replace rename into the no-follow-opened destination directory. Staging and output must be on the same filesystem; failures leave no partial destination file.

Skill layer roots and their components are opened without following symlinks. Department prompts require an entry in authoritative `org.toml` and a matching `agent.toml` mirror; unavailable or drifted data excludes the department layer. The office migration completion marker is written only after all four company skill directories pass the script manifest. Existing conflicting copies are preserved, ignored for execution, logged and shown by `duduclaw doctor`; an old migration marker does not certify them.

The existing MCP permission, approval, disabled-skill and document-limit gates remain in place. `office_script` supports Linux and macOS only and now fails closed unless filesystem isolation, network denial and memory limiting are available. Linux requires bwrap (read-only runtime/input mounts, private writable output/cache, network/PID namespaces) and a 2 GiB RLIMIT_AS. macOS uses a generated deny-by-default sandbox-exec profile, denies network access, restricts file access, and attempts DATA/RSS limits with an allocation-denial probe; unsupported or ineffective limits refuse execution. CPU/file/process/fd limits and process-group cleanup remain. A host-only warm step runs the verified wrapper with `--help` in a separate temporary directory with no user files; only that step may download dependencies into `<home>/.office-execution/uv-base-v1/`. The sandbox mounts this base read-only and uses an independent copy at `<private-workspace>/uv-cache` via `UV_CACHE_DIR`, `UV_LINK_MODE=copy` and offline mode. Cache links are rebased into the copy; script environments are rebuilt offline. The disposable workspace, staging and cache are removed after publication or failure. Legacy parser-writable caches are never reused as a trusted base; explicit unsandboxed fallback retains its existing cache behavior. The existing `config.toml [container.sandbox] script_when_unavailable = "run_unsandboxed"` explicitly permits fallback when isolation is unavailable, with one warning per host process; `sandbox_enabled = false` does not implicitly permit it. Same-UID pathname/cache races and the absence of a complete transitive-dependency hash lock remain documented in [Security defense](05-security-defense.md). Positive native Linux/macOS sandbox, real LibreOffice and gateway/CLI integration acceptance remain separate; local sandbox-exec was denied by the development environment.

Before creating the Linux mounts or macOS profile, the runner canonicalizes home, workspace/output, input, wrapper, base/run caches and every existing runtime allow-root. Equality or ancestor overlap in either direction (including runtime aliases and their canonical targets) refuses isolation; resolution errors also fail closed. The base cannot overlap the writable workspace, and the disposable cache must be inside that workspace. This prevents custom homes under `/usr/local` or `/opt/homebrew` and relocated private staging/cache paths from exposing other employees through broad runtime reads. Only the existing explicit `script_when_unavailable = "run_unsandboxed"` opt-out permits fallback, with the same warning.

The runner now pins direct dependencies, applies a fixed upload cutoff, forbids source builds/Python downloads, and uses offline uv execution after an input-free host warm step. System Python must match the direct pins. Unix resource limits and concurrent 32 KiB pipe caps terminate oversized/timed-out process groups; pre-spawn path/hash checks narrow the snapshot race. Complete dependency hashes and same-UID host-process isolation remain outside this boundary; explicit unsandboxed fallback also forfeits network denial. See [Security Defense](05-security-defense.md#office-parser-limits-and-remaining-trust-boundary) for exact limits, cache recovery and residual risks.

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

The reply text is the one hand-off point every runtime shares — Claude CLI, Codex, direct API. A marker in the reply lets the *model* declare intent while the *gateway* keeps authority over what actually leaves the sandbox. The alternative — the gateway inferring deliverables from prose — is exactly the failure mode the protocol exists to avoid.

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
- `docx/xlsx/pptx/odt/ods/odp/csv` are converted to PDF by LibreOffice (`soffice --headless`) from a verified snapshot in a private directory for each request, with an isolated LO profile and a 60-second timeout. The mutable attachment path and old mtime cache are not used for conversion. Both source and converted preview are limited to 2 MiB; larger files remain downloadable up to 20 MiB. File downloads and inline previews also authorize the same bounded bytes they return. These attachment operations require Unix no-follow directory handles; other platforms refuse access.
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


### Attachments in conversation history

On Unix gateways, readable history attachments appear as cards with a name, size, type and download button. PNG, JPEG, GIF and WebP show thumbnails. PDF and Office files download for opening in your document viewer; history does not run LibreOffice conversion. The Files page keeps its separate preview flow.

Existing Markdown attachment links are resolved on the server. DELIVER sources outside the attachments folder work only when an exact, unambiguous host archive record points to a copy in that employee's attachments folder. Missing files, old paths from another home, ambiguous source versions and unsafe paths do not produce downloadable cards. The browser receives an opaque reference, never the stored host path.

Requests use the current dashboard JWT in the Authorization header, without tokens in URLs. Both the conversation history rules and the existing artifact audience/ownership rules apply on every request. Masked external histories never expose original attachments. Restricted readers need trusted WebChat ownership or an existing task grant; an old message with a typed path is not an ownership grant. Handles expire after 30 minutes or a gateway restart: reload history to refresh them. Non-Unix gateways currently refuse this new file-serving route because descriptor-relative no-follow reads are not implemented there.

Downloads are limited to 20 MiB and image previews to 2 MiB. Larger images remain download-only; they are not fetched automatically as cards approach the viewport. The gateway checks the opened file's size, makes a bounded snapshot and checks its hash against recorded provenance and any required workflow receipt before returning those same bytes. Responses carry `Content-Length`; the browser checks that length and stops reading if the response exceeds its limit. Uploads use unpredictable UUID names and exclusive creation, so simultaneous same-name uploads do not overwrite each other.

History reads select the newest rows with SQL `LIMIT`, open the durable authorization index once per request and query archive/source keys. Each response is bounded to 2 MiB of message text, 1 MiB per message, 32 attachments and 256 inspected references. History projection allows at most two concurrent requests and 30 requests per minute per user, with four concurrent requests globally. Downloads allow two concurrent requests per user and four globally; each user has a 120 MiB-per-minute budget, reserved using the request's size cap rather than the bytes ultimately transferred. Exceeding a limit makes that request unavailable; retry later.

Attachment handles have a separate cache with a 512-entry per-user LRU quota. Capacity pressure can expire older handles before their normal 30-minute expiry. If the global cache cannot retain a handle, no unusable handle is returned. Restricted readers cannot download legacy artifacts without a recorded content hash; existing Admin/full-history rules for legacy files remain. Windows and other non-Unix gateways refuse this endpoint. Rust gateway tests on the remote builder, native platform behavior and live browser/download acceptance remain unverified for this repair.

WebChat validates the complete upload batch before writing files or dispatching a turn: at most 8 attachments and 20 MiB of decoded data, filenames up to 255 bytes and MIME labels up to 128 bytes. WebSocket messages and frames are capped at 28 MiB; a bounded attachment parser rejects excessive counts before constructing the attachment vector. Invalid base64 or an exceeded limit rejects the whole batch. Per-user admission allows 8 batches, 32 files and 80 MiB per minute. Durable reservations cap new uploads at 512 MiB / 2,048 files per user and 4 GiB / 16,384 files globally. Empty files still consume file quota. Failed or interrupted reservations remain charged; manually deleting a file does not restore this quota automatically.

Download responses have a 30-second total deadline. An independent timer releases the snapshot and admission permits even when the client never consumes the body. The transport receives copied chunks of at most 64 KiB, so a held chunk cannot retain the whole snapshot. Expired responses fail; retrying requires a new authorized request and consumes its normal budget.

Historical attachment authorization uses an indexed durable SQLite store for archive name, owner, content hash, size and source path. Rotating or truncating the display/audit JSONL no longer removes existing authorization records. Database failures refuse restricted access, and WebChat uploads must save their authorization record before reporting success. Legacy files without a trustworthy content binding remain unavailable to restricted readers.

On first use, the store imports the retained previous and current audit files in one transaction, with a combined 32 MiB limit and 64 KiB per row. Unreadable, malformed or oversized input aborts migration without partial grants; repair the retained records before retrying. Records already discarded by older rotations cannot be reconstructed automatically. Later audit edits do not overwrite the imported authority store.
