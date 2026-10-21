# Document knowledge base

Upload company documents (handbooks, price lists, product FAQs) into **collections**, decide per collection who may read them, and let AI employees search them with citations to the page, slide or spreadsheet rows they came from.

Status in this release: storage, ingestion, keyword search, the `knowledge.*` RPCs, the upload route, the three read-only MCP tools and the `duduclaw knowledge eval` command. The dashboard pages, vector (semantic) search, automatic injection into channel replies and citation footers come in later releases. The existing wikis are not touched.

---

## Collections and permissions

A collection is the unit of permission. Every grant names a principal and a role:

| Principal | Id | Roles | Who it covers |
|---|---|---|---|
| user | dashboard user id | viewer, editor, owner | that person |
| department | department name, exact match | viewer, editor | people whose account has that department, and AI employees with `[agent] department` set to it |
| agent | AI employee id | viewer | that AI employee |
| channel | `line` (the whole channel) or `line:<chat id>` | viewer | **anyone** talking in that channel or chat, including people with no dashboard account |

- **viewer** searches, reads passages, downloads originals and lists sources. **editor** also adds, deletes and re-indexes sources. **owner** also changes permissions and settings and deletes the collection.
- Admins are owners of every collection. Managers can create collections and become their owner; Employees cannot create collections.
- A new collection is visible to its creator only. A grant that cannot be read or parsed counts as no grant. A collection must keep at least one user owner.
- A `channel` grant is meant for public material such as an FAQ: whoever adds the bot as a friend can read it.

### What one search may read

For a dashboard user, the collections they hold any role on (Admin: all).

For an AI employee answering someone, the intersection of:

1. **The employee side.** Collections granted to the employee itself or to its department, narrowed by its `agent.toml`:

   ```toml
   [knowledge]
   collections = ["hr-handbook", "product-faq"]   # slugs; [] turns the tools off
   ```

   No `[knowledge]` section means every collection granted to the employee. Unknown slugs are ignored. A section the gateway cannot parse counts as `[]`. Employees cannot edit this section themselves.

2. **The person side.** Who the turn is for: a signed-in user brings their user and department grants; an unidentified messaging-app sender brings only `channel` grants; a turn with no person (cron, heartbeat, dispatch) may use public and internal collections granted to the employee.

3. **Sensitivity.** Collections are `public`, `internal` (default), `personal` or `restricted`. Outside a 1:1 conversation (group chats, and every turn whose privacy cannot be proven) `personal` and `restricted` collections are excluded.

4. **Narrowing.** The `collections` argument of `knowledge_search` can only shrink the set.

In this release the gateway does not yet know which signed-in person a WebChat or channel turn belongs to (that arrives with the trigger-routing work). Until then a tool call made during a live channel or WebChat turn is treated as an unidentified sender of that chat (channel grants only), and a call with no live turn as a turn with no person. This is narrower than the final behaviour, never wider.

## Adding documents

| How | Who | Notes |
|---|---|---|
| `POST /api/knowledge/upload` | editor | multipart: a `collection_id` field first, then 1 to 20 `file` parts; Bearer token of a dashboard user |
| `knowledge.sources.add` `{kind: "paste"}` | editor | pasted text, stored as a `.txt` source |
| `knowledge.sources.add` `{kind: "url"}` | editor | a public `http`/`https` page, fetched when the job runs (never cached) |

Formats: PDF with a text layer, Word `.docx`, Excel `.xlsx` / `.xls`, PowerPoint `.pptx`, CSV, plain text and Markdown; web pages for URL sources. Old `.doc` / `.ppt`, password-protected files, images and audio are refused with a message that says how to convert them. Scanned PDFs have no text layer and are refused (no OCR yet); pages without text inside an otherwise readable PDF are listed as a warning.

The same file uploaded twice to one collection answers `duplicate` and returns the existing source. Different collections can hold the same file.

A source moves `queued → extracting → indexing → ready`. While there is no vector model, `ready` carries `status_detail = "keyword_only"`. Failures keep a closed-set code and a message for the uploader:

| Code | Meaning |
|---|---|
| `too_large`, `too_many_chars` | over `max_source_bytes` or `max_source_chars`; the whole document is refused, never truncated |
| `quota_sources`, `quota_chunks`, `quota_collections` | a configured ceiling is reached |
| `unsupported_format`, `legacy_office`, `encrypted`, `no_text_layer`, `empty_text`, `encoding_unknown` | the file cannot be read as text |
| `document_limits` | an Office file failed the zip-bomb and nesting checks |
| `extract_timeout`, `extract_crashed`, `extract_oom` | the extraction process ran out of time, crashed or ran out of memory |
| `url_blocked`, `url_fetch_failed`, `url_too_large` | the page is internal, unreachable, not HTML or text, or over 5 MB |
| `retry_exhausted` | the gateway stopped mid-job three times |

A failed re-index keeps the previous index searchable and shows the error next to it.

### How a file is read

Parsing never happens inside the gateway. Each file is read by a child process, `duduclaw knowledge-extract`, started from the same binary with an empty environment:

- a wall-clock limit (`extract_timeout_secs`, default 180 s), then the process group is killed;
- on macOS and Linux a CPU-time limit and no core dumps, and on Linux an address-space limit (`extract_memory_mb`, default 2048);
- on macOS (Seatbelt) and Linux (Landlock) an OS sandbox that allows no writes and no network;
- Office files pass the existing zip-bomb and XML-depth checks first;
- output over `max_source_chars × 4` bytes is cut off and refused.

Windows has neither the sandbox nor the memory limit, only the time limit; such sources carry an `extract_unsandboxed` warning. Only one extraction runs at a time.

Text is normalised with NFKC (full-width letters, digits and punctuation become half-width), then cut into passages of about 500 characters with some overlap, never across PDF pages (a short page tail joins the next page's first passage) and, for spreadsheets, only at row boundaries with the header row kept with every passage. Every passage is checked for prompt-injection wording; hits are marked and shown with a warning.

## Search

Keyword search only in this release:

- Terms of three or more characters go through a SQLite FTS5 trigram index. Chinese queries are split at particles such as 的、了、嗎 so 「請假的規定」 still finds 「請假規定」.
- Two-character terms (「請假」, `HR`) use a bounded substring scan of up to 20,000 passages; the result says `scan_capped` when the scan hit the ceiling.
- When the trigram list finds fewer than three passages, the two-character pieces of longer words join the substring scan.
- The lists are merged with reciprocal rank fusion, at most three passages per source.

Every result says the search mode and why vector search is off. Queries phrased differently from the document (「薪水」 for 「薪資」) are missed until vector search arrives.

## For AI employees

Three read-only MCP tools, scope `knowledge:read` (not grantable to external MCP keys):

| Tool | Arguments | Answer |
|---|---|---|
| `knowledge_search` | `query`, `collections` (slugs, narrowing only), `limit` (1–20, default 5) | `[n] title · location (collection) chunk_id=…` with the passage text, marked as document content |
| `knowledge_get_passage` | `chunk_id`, `context` (0–2) | the passage with the text before and after it |
| `knowledge_list_collections` | — | the collections this turn may read, with source and passage counts |

They are listed whenever the knowledge base is enabled and the employee's `[knowledge]` section does not turn them off. An employee with no grants still sees them and is told there is nothing to read. The tools call the gateway over its signed loopback route; when the gateway is down they report that and never read the database directly.

## Audit

- Every search, passage read, listing and download writes a row to `knowledge.db`'s access log: who (dashboard user, employee, channel), the query (first 500 characters), the allowed collections and the result ids, never the passage text. Rows older than `access_log_retention_days` are deleted hourly.
- Creating, changing and deleting collections, permission changes and source deletions are also written to `security_audit.jsonl`.
- Collection owners read the access log with `knowledge.access_log.list`.

## Settings (`config.toml`)

```toml
[knowledge]
enabled = true
max_source_bytes = 104857600     # 100 MB per file; 1 MB–200 MB (restart to resize the upload limit)
max_source_chars = 500000
max_sources_per_collection = 300
max_collections = 200
max_chunks_total = 200000
extract_timeout_secs = 180
extract_memory_mb = 2048         # Linux only
access_log_retention_days = 180
```

Out-of-range values are clamped. `enabled = false` hides the tools and refuses every RPC and upload.

## Checking retrieval quality

```
duduclaw knowledge eval --docs <folder> --questions <questions.jsonl> [--k 5] [--min-recall 0.85] [--report out.json]
```

Builds a throw-away database in a temporary folder, ingests the folder with the same extraction process and indexing the gateway uses, and reports Recall@1/5/10 and MRR@10 per question type, how often unanswerable questions get no result, and query latency. Exit code 1 when Recall@k is below `--min-recall`. Question lines look like:

```json
{"id":"q01","question":"到職滿一年的特休有幾天？","type":"factoid","gold":[{"source":"員工手冊.md","contains":"特別休假七日"}]}
{"id":"q13","question":"公司有員工宿舍嗎？","type":"unanswerable","answerable":false}
```

A gold entry can also name `page`, `slide`, `sheet` and `row`. A small fictional set lives in `crates/duduclaw-knowledge/tests/fixtures/eval/`; keep real customer documents and questions out of the repository.

## Data and deletion

Everything lives under `<home>/knowledge/`: `knowledge.db` and the original files under `blobs/`, named by their SHA-256. Deleting a source removes its passages, index rows and vectors in one transaction and the original file when no other collection uses it; the database runs with `secure_delete` on. Deleting a collection requires typing its name.

## Related

- [MCP tools](../guides/mcp-tools.md)
- [Security defense](05-security-defense.md)
- [Dashboard settings](../guides/dashboard-settings.md)
