# Document knowledge base

Upload company documents (handbooks, price lists, product FAQs) into **collections**, decide per collection who may read them, and let AI employees search them with citations to the page, slide or spreadsheet rows they came from.

Available now: storage, ingestion, keyword search, meaning-based (vector) search with a local model or an optional cloud provider, the `knowledge.*` RPCs, the upload route, the MCP tools (three read-only, one approval-gated proposal), the `duduclaw knowledge eval` command, the dashboard's Document library pages with the AI agent binding, and automatic retrieval into channel and WebChat replies with `[n]` citations, the 「加入知識庫」 chat command and forget-by-source on delete. The existing wikis are not touched.

---

## Using the dashboard

**Document library** in the sidebar's Company group (on Personal, in the main list right after Memory) lists the collections you can see: name, sensitivity, your role, the number of documents and how many are still being read.

1. **Create a collection**: managers and admins press "New collection" and give a name, description and sensitivity; the short code can be left empty and is generated (it is the code AI agent settings use). Only you can see a new collection.
2. **Add sources**: on the collection's Sources tab press "Add source" to drop or choose files (up to 20 at a time; a file of an unsupported type or over the size limit is stopped before it is sent, with the reason; with an upload progress bar and then one line per file: added, already there, or why it was refused), paste text, or add a public web page. Each document carries its state: queued, reading, indexing, ready (word match), waiting for approval, failed (with the reason). While a document is still being read the list refreshes every 3 seconds. A document's menu downloads the original, reads it again or deletes it (the last two need the editor role).
3. **Try a search**: type a question and see which passages of this collection come back, in order, with the page or section of each; a result opens the original text in a side drawer with the passage highlighted, and you can step to the previous or next passage.
4. **Sharing** (owners only): the picker on the right has People, Departments, AI agents and Channels and lists only principals you may share with: People and Departments exist on Enterprise only, and people are found by a search of at least 2 characters (at most 20 shown, never the whole list); AI agents are the ones you own (admins: all; agents you can only view or operate do not count); a channel can be shared whole or limited to one chat by its ID. The role menu disables roles a kind of principal cannot hold, and the last owner cannot be removed. Sharing a whole channel shows a warning. "Save sharing" saves the list in one go and asks you to reload when someone else saved in between. The gateway applies the same rule when saving: a principal you add, or one whose role you raise (viewer to editor, for example), is refused when you may not share with it; existing principals kept at the same or a lower role are left alone.
5. **Access record** (owners): when, who (a person, an AI agent acting for a person, someone without an account on a channel), what they did, what they asked and how many passages came back.
6. **Settings** (owners): name, description, department label, sensitivity, "Process on this computer only" (later meaning-based search never sends this collection to an outside service), and deleting the collection after typing its name.

Admins also get "Refused reads" at the top of the Document library page: every refused read across all collections.

**Letting an AI agent use a collection**: share the collection with the agent (or its department) on the collection's Sharing tab, then open Agents → Edit Settings → Tools & permissions → Document library and choose: every collection shared with it, only the ticked ones, or none. This writes `agent.toml [knowledge] collections`; only collections shared with the agent can be ticked, so the setting can only narrow. The agent's owners and admins can change it.

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

   No `[knowledge]` section means every collection granted to the employee. Unknown slugs are ignored. A section the gateway cannot parse counts as `[]`. Employees cannot edit this section themselves; their owners and admins set it in the dashboard (see "Letting an AI agent use a collection" above).

2. **The person side.** Who the turn is for, resolved once per reply by the gateway:

   | Turn | Person | Grants that count |
   |---|---|---|
   | WebChat, signed-in dashboard account | that account (never taken from the session id) | user, department, Admin: all |
   | messaging app (LINE, Telegram, …), sender with a **verified** channel binding to an active account | that account | user, department, plus the chat's `channel` grants |
   | messaging app, any other sender | nobody | `channel` grants only |
   | public WebChat widget visitor, Odoo website Live Chat visitor | nobody | `public` collections granted to `channel:widget` / `channel:odoolivechat` only (an internal collection granted to these channels is still not readable), never 1:1 |
   | cron runs, and dispatched runs with no upstream turn (goal rounds, dashboard work) | nobody (system turn) | public and internal collections granted to the employee |
   | a run dispatched from a channel or WebChat turn (an employee asked another one) | the upstream turn's person | that person's grants outside a 1:1 conversation; for an unbound sender or visitor, that channel's grants only. Delegation never widens access; an upstream turn the gateway no longer remembers (restart, over 24 hours) reads nothing |
   | a tool call whose turn and run the gateway did not register, or a reply turn from an unrecognised session | nobody | none |

3. **Sensitivity.** Collections are `public`, `internal` (default), `personal` or `restricted`. Outside a 1:1 conversation (group chats, and every turn whose privacy cannot be proven) `personal` and `restricted` collections are excluded.

4. **Narrowing.** The `collections` argument of `knowledge_search` can only shrink the set.

Heartbeat runs and other model calls that carry neither a turn id nor a run id read nothing (narrower than a system turn, never wider).

## Adding documents

| How | Who | Notes |
|---|---|---|
| `POST /api/knowledge/upload` | editor | multipart: a `collection_id` field first, then 1 to 20 `file` parts; Bearer token of a dashboard user |
| `knowledge.sources.add` `{kind: "paste"}` | editor | pasted text, stored as a `.txt` source |
| `knowledge.sources.add` `{kind: "url"}` | editor | a public `http`/`https` page, fetched when the job runs (never cached); decoded with its declared charset (Big5, GBK, …); the URL is normalised (no fragment, lower-case host, no trailing `/`) so the same page is a duplicate |

Formats: PDF with a text layer, Word `.docx`, Excel `.xlsx` / `.xls`, PowerPoint `.pptx`, CSV, plain text and Markdown; web pages for URL sources. Old `.doc` / `.ppt`, password-protected files, images and audio are refused with a message that says how to convert them. Scanned PDFs have no text layer and are refused (no OCR yet); pages without text inside an otherwise readable PDF are listed as a warning.

The same file uploaded twice to one collection answers `duplicate` and returns the existing source. Different collections can hold the same file.

A source moves `queued → extracting → indexing → ready`, and then through `embedding` back to `ready` while its vectors are computed (it is searchable by keywords the whole time). `status_detail` says what search covers it: `hybrid` (vectors done), `keyword_only` (no usable model, or vectors failed; the source then carries an `embedding_failed` warning and is retried by the next backfill) or `vector_limit` (`max_vector_rows` reached). Failures keep a closed-set code and a message for the uploader:

| Code | Meaning |
|---|---|
| `too_large`, `too_many_chars` | over `max_source_bytes` or `max_source_chars`; the whole document is refused, never truncated |
| `quota_sources`, `quota_chunks`, `quota_collections`, `quota_bytes` | a configured ceiling is reached |
| `unsupported_format`, `legacy_office`, `encrypted`, `no_text_layer`, `empty_text`, `encoding_unknown` | the file cannot be read as text |
| `document_limits` | an Office file failed the zip-bomb and nesting checks |
| `extract_timeout`, `extract_crashed`, `extract_oom` | the extraction process ran out of time, crashed or ran out of memory |
| `url_blocked`, `url_fetch_failed`, `url_too_large` | the page is internal, unreachable, not HTML or text, or over 5 MB |
| `sandbox_unavailable` | the OS sandbox for extraction is not available on this host (see below) |
| `retry_exhausted` | the gateway stopped mid-job three times |

A failed re-index keeps the previous index searchable and shows the error next to it.

### How a file is read

Parsing never happens inside the gateway. Each file is read by a child process, `duduclaw knowledge-extract`, started from the same binary with an empty environment:

- a wall-clock limit (`extract_timeout_secs`, default 180 s), then the process group is killed;
- on macOS and Linux a CPU-time limit and no core dumps, and on Linux an address-space limit (`extract_memory_mb`, default 2048);
- on macOS (Seatbelt) and Linux (Landlock) an OS sandbox that allows no writes, no network, and reads only of the file itself, the extractor binary and the system library directories;
- Office files pass the existing zip-bomb and XML-depth checks first;
- output over `max_source_chars × 4` bytes is cut off and refused.

Where the sandbox is not available (Windows, or confinement fails) the file is refused with `sandbox_unavailable`. An operator who accepts the risk can set `extract_when_unavailable = "run_unsandboxed"`; such runs keep the time limit (macOS and Windows have no memory limit) and the source carries an `extract_unsandboxed` warning. `.xlsx` sheets are read cell by cell with a ceiling of 2 million non-empty cells, so a sheet declaring a huge size cannot exhaust memory. Only one extraction runs at a time.

Passages keep the document's own characters (full-width punctuation stays full-width). For matching only, a second copy of each passage is folded with NFKC and lower case (full-width letters and digits, Kangxi radicals from PDF fonts), so 「ＨＲ」 and 「HR」 find each other. Text is cut into passages of about 500 characters with some overlap, never across PDF pages (a short page tail joins the next page's first passage) and, for spreadsheets, only at row boundaries with the header row kept with every passage. Every passage is checked for prompt-injection wording; hits are marked and shown with a warning.

## Search

Three ranked lists, each filtered to the collections the caller may read before anything is ranked, merged with reciprocal rank fusion (`Σ 1/(60 + rank)`, ties by passage id, at most three passages per source):

- **Trigram**: terms of three or more characters go through a SQLite FTS5 trigram index. Chinese queries are split at particles such as 的、了、嗎 so 「請假的規定」 still finds 「請假規定」.
- **Short terms**: two-character terms (「請假」, `HR`) use a bounded substring scan of up to 20,000 passages; the result says `scan_capped` when the scan hit the ceiling. When the trigram list finds fewer than three passages, the two-character pieces of longer words join the scan.
- **Vectors** (meaning-based): the question is embedded with the same model as the passages and compared by cosine similarity with every passage of the allowed collections (brute force over an in-memory copy, reloaded whenever passages or vectors change). This is what finds 「小孩生病要在家顧可以請什麼假」 in a passage about 家庭照顧假.

Every result says the mode (`hybrid`, `keyword_only`, `vector_only`), the share of searched passages that have vectors (`coverage`) and, when vectors were not used, why (`vector_unavailable`): no model installed, model files that fail their check, an unsupported platform, a cloud provider without the disclosure acknowledgement, redaction switched off, a missing API key, a cloud error, or every allowed collection being kept off the cloud. Search then runs on keywords only; it never switches to another provider on its own. The gateway also logs the reason (at most once an hour per reason).

### Embedding models

| | multilingual-e5-small (default) | Qwen3-Embedding-0.6B (optional) |
|---|---|---|
| Runs | on this machine (ONNX Runtime 1.24.2) | on this machine (ONNX Runtime 1.24.2) |
| Files | `intfloat/multilingual-e5-small` @ `614241f6`: int8 ONNX 118 MB + tokenizer 17 MB | `onnx-community/Qwen3-Embedding-0.6B-ONNX` @ `c25a394d`: int8 ONNX 614 MB + tokenizer 11 MB |
| Licence | MIT | Apache-2.0 (base model `Qwen/Qwen3-Embedding-0.6B`; the conversion repository declares none of its own) |
| Vector | 384 dimensions, mean pooling, `query: ` / `passage: ` prefixes | first 512 of 1,024 dimensions, last-token pooling, an English instruction before the question |
| Speed (Apple M4, 5 threads) | about 1,200–2,500 passages per minute | about 150–340 passages per minute |

Every file is pinned to one commit with its size and SHA-256; nothing resolves "latest". Models are not part of the binary: an Admin installs one from **Settings → Knowledge base search** (the ONNX Runtime library, about 30 MB, is installed with it when missing, shared with the AI detection of redaction). After an install every passage gets a vector in the background; a re-indexed source keeps the vectors of passages whose text did not change. A model unused for `idle_unload_minutes` is unloaded. Models with a non-commercial licence (jina-embeddings v3 / v5) are not offered. The Qwen3 conversion is published without a licence statement of its own; its Apache-2.0 terms come from the base model, so check them yourself before commercial use. The ONNX Runtime library is pinned per platform as well and is hashed before it is loaded, for the knowledge base and for the AI detection of redaction alike: a library placed by hand is refused. macOS on Intel has no ONNX Runtime build at this version and shows "unsupported".

**Offline machines**: each release carries `duduclaw-kb-model-multilingual-e5-small-<platform>.tar` and its minisign signature `.tar.minisig`, signed with the release key the self-updater trusts (`scripts/kb-model-bundle.sh <platform> <model> <dir>` builds a bundle for any catalog model; `KB_BUNDLE_MINISIGN_KEY` signs it). Copy both files, still packed, into `models/embedding/` inside the DuDuClaw folder and press **Check again (offline install)**. DuDuClaw checks the signature (and that it names this file) before it reads the tar, unpacks only the expected regular files (links, other paths or extra files refuse the whole bundle), hashes every file against its pin, checks the runtime library against its own pin, and only then counts the model as installed; the bundle files are deleted afterwards. An unsigned bundle, or one signed by another key, is refused.

### Cloud providers

`provider = "openai"` (`text-embedding-3-small`, 1,536 dimensions) or `"gemini"` (`gemini-embedding-001`, 768 dimensions) computes vectors at the provider instead. Rules:

- An Admin chooses it in the dashboard and must tick that passages (after de-identification) go to that vendor and host. The acknowledgement (who, when, provider, host, port, model) is stored in `config.toml`; a record written before ports were recorded covers only port 443, so an endpoint on another port needs a new acknowledgement; changing the provider, endpoint or model needs a new one, and choosing anything else removes it.
- Every passage and every question goes through the operator's `[redaction]` rules first; each hit becomes a marker such as `[EMAIL]` and nothing is restored. Redaction switched off or force-disabled, rules that do not compile, or no rule at all: nothing is sent, search stays keyword-only. Rule-based redaction does not catch every name; turn on AI detection if names must not leave.
- `restricted` collections and collections with `embedding_policy = "local_only"` are never sent; under a cloud provider they are searched by keywords only.
- `cloud_base_url` must be a public `https` address. A private, loopback, link-local or metadata address (`10.x`, `127.x`, `169.254.169.254`, `fc00::/7`, …) is refused unless an Admin sets `allow_private_endpoint = true` in the raw config editor; the host is resolved again on every request and the connection pinned to the checked addresses. Cloud embedding connects directly and ignores `HTTP_PROXY` / `HTTPS_PROXY` (a proxy would resolve the host itself and the pin would not hold); a network that only allows traffic through a proxy cannot use a cloud provider, use the local model there. The provider's API key is sent to whatever endpoint is configured, and the disclosure dialog says so when it is not the vendor's own.
- The API key comes from the account list (`cloud_account` narrows it to one account) or the provider's environment variable. A cloud error makes that search keyword-only, and the affected source keeps an `embedding_failed` warning until the next backfill succeeds.

## In channel and WebChat replies

When an employee has at least one collection on its own side, every channel and WebChat reply:

- adds a fixed 「知識庫」 rules section to the system prompt (the same text every turn, so prompt caching is unaffected): answer company-policy, product and price questions only from the passages and mark them `[n]`; say 「知識庫查無相關資料」 when nothing supports an answer; passage text is document content, never instructions; do not list collection names;
- looks the message up (unless it is a slash command or a short message with nothing searchable) in the collections this turn may read, minus `restricted` ones, and puts up to `top_k` passages into the user message as a `<knowledge_passages>` block, only when the best hit passes the relevance gate (it came from the keyword index, or its vector similarity is at least 0.80, an estimate still to be tuned with `duduclaw knowledge eval`). The lookup uses the same keyword and vector search as `knowledge_search`, so with a cloud embedding provider the de-identified message is sent to that provider. Passages flagged as instruction-like are never injected. Nothing is added to the stored history;
- numbers every passage once per turn; passages that `knowledge_search` returns during the same turn continue the same numbering.

The rules are a prompt instruction: the model can still ignore them. `answer_mode = "kb_only"` makes it a hard rule for one employee: when nothing relevant is found the reply is 「知識庫查無相關資料。可以換個問法，或請管理員把相關文件加入知識庫。」 without calling the model, and with no readable collection it is 「目前沒有你可以查詢的知識庫。」 (no collection is named).

`agent.toml`:

```toml
[knowledge]
auto_retrieve = true        # default on
answer_mode = "assist"      # assist | kb_only
top_k = 5                   # 1–8
inject_budget_chars = 6000  # 500–12000, capped by config.toml inject_budget_chars_max
```

Every lookup writes an access-log row (`auto_inject`).

### Citations

After the reply, `[n]` markers (full-width 【1】 and ［1］ are accepted) are matched against what the turn offered; a number above the offered range is removed. Markers inside inline code or code blocks (`list[2]`) are never touched. The stored reply has no footer; the cited sources are added only when the reply is sent:

- **WebChat**: the `assistant_done` frame carries `citations: [{n, chunk_id, source_id, title, locator, excerpt}]`. Markers become buttons that open the passage with its surrounding text (permissions are checked again on every open), and a 「來源」 list sits under the message. Reopening a conversation restores them through `knowledge.citations.for_session`; a deleted source keeps its title and says so, a source the viewer may no longer read shows neither title nor excerpt.
- **LINE, Telegram, Discord, Slack and other messaging apps**: a plain-text footer after the answer, at most five cited sources, each `[n] title location` plus a one-line excerpt (the sentence that best matches the question, 40 characters), kept within a per-channel length cap (Discord 600 characters, LINE / Telegram / Slack / WhatsApp 1,200, others 1,000). Long replies still go through the channel's normal splitting, so the footer ends up in the last message.

Each turn's offered and cited passages are kept in `turn_citations` (title, location and a short excerpt; no passage text). Deleting a source (or its collection) blanks the title, location and excerpt of its rows; the ids stay for forget by source. Titles, locations and excerpts in footers have `@`, `<` and `>` replaced by their full-width forms, so a document named `@everyone` or `<!channel>` mentions nobody.

In a group chat a bound sender's own and department collections can be quoted: the answer and its footer (title, location, one-line excerpt) are visible to everyone in that group. Personal and restricted collections are never used there.

## For AI employees

Four MCP tools. The three read-only ones use scope `knowledge:read`, `knowledge_add_source` uses `knowledge:write`; neither scope can be granted to external MCP keys:

| Tool | Arguments | Answer |
|---|---|---|
| `knowledge_search` | `query`, `collections` (slugs, narrowing only), `limit` (1–20, default 5) | `[n] title · location (collection) chunk_id=…` with the passage text, marked as document content |
| `knowledge_get_passage` | `chunk_id`, `context` (0–2) | the passage with the text before and after it |
| `knowledge_list_collections` | — | the collections this turn may read, with source and passage counts |
| `knowledge_add_source` | `collection` (slug), exactly one of `path` (a file in the employee's own `attachments/`), `url`, `text`; `title` | an approval card id; nothing is added until a person approves it |

They exist for an employee only while at least one collection is on its side (granted to it or its department, narrowed by its `[knowledge]` section) and the knowledge base is enabled; the listing and the call use the same check, and granting or revoking a collection updates an open session (`notifications/tools/list_changed`). Results put every passage, collection name and description inside a marked block whose lines start with `│ `, so text inside a document cannot pass for a result header or end the block. The tools call the gateway over its signed loopback route; when the gateway is down they report that and never read the database directly.

## Proposing documents from chats and employees

Two ways to add a document from outside the dashboard, both **always** approval-gated (there is no setting that skips it):

- An employee calls `knowledge_add_source` for a collection on its own side. `path` must be a file in that employee's own `attachments/` folder. The turn must be for a signed-in person, a sender with a verified binding, or a system run; in a visitor's, an unbound sender's or an unregistered turn the tool refuses.
- A person types 「加入知識庫 <collection slug>」 in a chat. The files the channel adapter saved for that sender in that conversation are proposed: those attached to the message, or, when it has none, the ones the same sender attached there during the last 30 minutes (at most five). A file reference typed into a message names nothing, and the card uses the stored file name. After a gateway restart earlier attachments are not accepted; send the file again. Only a signed-in WebChat account or a messaging-app sender with a verified binding who can read the collection may propose; a missing collection and one the person cannot read get the same answer.

The card (`knowledge_add_source`) is decided only in the dashboard, only by an Admin or a person who is editor or owner of that collection at the moment of the decision; channel buttons and replies are refused. The card carries the collection, title, size, the content's SHA-256, who asked (the turn's kind of person and channel) and, for plain text, Markdown, CSV and HTML, the first 2,000 characters as a preview (shown as text; binary files show name, type and size). The content waits in a separate owner-only file under `<home>/knowledge/proposals/`; files no pending card refers to are removed. At most 20 cards may wait per employee and 10 per proposer. Approving re-hashes that file; if it changed since filing the card is closed as denied and nothing is ingested. An approved proposal becomes a normal ingest job; a denied or expired one deletes the waiting file. Cards expire after three days. Admins and Managers see these cards in the approvals inbox; any collection editor (an Employee included) can list and decide the cards of their collections with `knowledge.proposals.list` and `knowledge.proposals.decide {id, approve}` (the Document library pages do not show them yet; this is RPC only for now). The approvals inbox shows these cards only to Admins and to editors of that collection. An editor may approve a card they proposed themselves from a chat; that is the same as uploading the file in the dashboard, and the card and the source keep who proposed and who approved.

## Forgetting what employees learned from a document

`knowledge.sources.delete` with `also_forget_memories: true` looks up the conversations whose replies **cited** the source and files one forget-by-source plan per employee and conversation (at most 20 per deletion; the rest are reported as skipped), covering those turns' messages. The plans are filed only after the deletion succeeded. Each plan waits for the existing Admin-only dashboard approval and is applied afterwards with `duduclaw memory forget-source apply`. This is coarse on purpose: it forgets everything those turns taught the employee, not only what came from the document. Passages that were offered but not cited are not included.

## Audit

- Every search, passage read, listing and download writes a row to `knowledge.db`'s access log: who (dashboard user, employee, channel), the query (first 500 characters), the allowed collections and the result ids, never the passage text. Rows older than `access_log_retention_days` are deleted every ten minutes.
- Creating, changing and deleting collections, permission changes and source deletions are also written to `security_audit.jsonl`.
- Collection owners read the access log with `knowledge.access_log.list`: who, when, which employee or channel, how many results, and a short hash of the question. The question text itself is shown only to Admins and to the person who asked.
- Admins list refused reads across all collections with `knowledge.access_log.denied`.
- The sharing picker uses `knowledge.principals.search` (display names and departments only, never e-mail addresses; people only for a search of at least 2 characters, at most 20; people already on the list are looked up by id through `resolve`); the upload pre-check uses `knowledge.limits`; the employee edit page reads the collections shared with an employee through `knowledge.agent.collections` (id, code, name and sensitivity only).

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
max_collection_bytes = 2147483648 # 2 GB of original files per collection
max_total_bytes = 21474836480     # 20 GB in total
extract_when_unavailable = "refuse"  # or "run_unsandboxed"
auto_retrieve = true             # automatic retrieval into replies, for every employee
inject_budget_chars_max = 12000  # ceiling on passages per reply (500–12000)

[knowledge.embedding]
provider = "local"                     # local | openai | gemini | none
local_model = "multilingual-e5-small"  # or "qwen3-embedding-0.6b"
cloud_model = ""                       # empty = the provider's default
cloud_account = ""                     # account id; empty = any account of that provider, then the env var
cloud_base_url = ""                    # empty = the official endpoint; public https only
allow_private_endpoint = false         # Admin-only: accept a private / loopback endpoint (http only to 127.0.0.1)
batch_size = 16
threads = 0                            # 0 = half the cores
max_vector_rows = 200000
idle_unload_minutes = 10
# cloud_disclosure_ack is written by the disclosure dialog only; the raw editor refuses to add or change it
```

Out-of-range values are clamped. `enabled = false` hides the tools and refuses every RPC and upload. An unknown `provider` value means keyword-only search, shown with the reason.

RPCs: `knowledge.embedding.status` (any signed-in user) and, Admin only, `.install {model}`, `.cancel`, `.remove {model}`, `.verify_offline {model}`, `.select {provider, local_model?, cloud_model?, cloud_account?, acknowledge_disclosure?}`, `.rebuild`. Installing, removing and changing the provider are written to `security_audit.jsonl`.

## Checking retrieval quality

```
duduclaw knowledge eval --docs <folder> --questions <questions.jsonl>
  [--model none|local:multilingual-e5-small|local:qwen3-embedding-0.6b] [--model-home <dir>]
  [--modes keyword,vector,hybrid] [--k 5] [--min-recall 0.85] [--report out.json] [--work-dir <empty dir>]
```

Builds a throw-away database in a temporary folder, ingests the folder with the same extraction process and indexing the gateway uses, and reports Recall@1/5/10 and MRR@10 per question type, how often unanswerable questions get no result, and query latency. With a `local:` model (installed under the DuDuClaw home or `--model-home`) every passage is embedded once and the report adds passages per minute, the vector size and peak memory, plus the `vector` (model only) and `hybrid` modes. Exit code 1 when Recall@k is below `--min-recall`. Question lines look like:

```json
{"id":"q01","question":"到職滿一年的特休有幾天？","type":"factoid","gold":[{"source":"員工手冊.md","contains":"特別休假七日"}]}
{"id":"q13","question":"公司有員工宿舍嗎？","type":"unanswerable","answerable":false}
```

A gold entry can also name `page`, `slide`, `sheet` and `row`. Two fictional sets live in the repository: `crates/duduclaw-knowledge/tests/fixtures/eval/` (14 questions) and `eval-zh50/` (5 documents, 50 questions including 8 paraphrases with no shared terms). On `eval-zh50` (Apple M4, 2026-10-22), Recall@5 was 0.78 for keywords, 0.96 hybrid with multilingual-e5-small and 0.93 hybrid with Qwen3-Embedding-0.6B, which is why e5 stays the default. Keep real customer documents and questions out of the repository.

## Data and deletion

Everything lives under `<home>/knowledge/`: `knowledge.db` and the original files under `blobs/`, named by their SHA-256. Deleting a source removes its passages, index rows and vectors in one transaction and the original file when no other collection uses it (a file uploaded in the last ten minutes is removed by the next cleanup instead, at most about twenty minutes later, because an upload of the same bytes may still be running); the database runs with `secure_delete` on. Deleting a collection requires typing its name.

## Related

- [MCP tools](../guides/mcp-tools.md)
- [Security defense](05-security-defense.md)
- [Dashboard settings](../guides/dashboard-settings.md)
