# Data sources and the native database connector

> A redaction rule can only mask a field it can find. This is how it finds one — in your own MCP servers, in a customer's, or in a database DuDuClaw connects to itself.

---

## The gap this closes

Until 2026-09, a `db_field` rule (`fields = ["res.partner.name", "hr.employee.*"]`) only ever worked against Odoo. The `model.field` syntax was sugar over a `json_path` rule, and the table it expanded against — which tool returns which table's records, where they sit in the JSON, whether the field names survive a mapper — was one hard-coded Rust table (`ODOO_TOOLS`). There was no way to point `db_field` at anything else.

There was a second, deeper gap underneath it. Redaction only ever intercepted DuDuClaw's own MCP server — a single choke point inside `duduclaw mcp-server`. A customer's **own** MCP servers (a Postgres MCP, a MySQL MCP, a 鼎新 ERP bridge, declared in the agent's `.mcp.json`) are spawned by the Claude CLI directly, so their tool results never reached the pipeline at all. Turning the Odoo table into something pluggable would not have helped a single one of those servers — the data never got to redaction to begin with.

This feature closes both: a **registry** any `db_field` rule can point at, and two ways for a data source's rows to actually flow through redaction — a **proxy** for MCP servers you already have, and a **native connector** for databases you don't have an MCP server for at all.

---

## Two kinds of data source

| Kind | Describes | Rows come from | Config |
|---|---|---|---|
| Tool-backed registry entry | Which MCP tools return which table's records, and where those records sit in the returned JSON | Any MCP server — DuDuClaw's own, or an external one reached through the MCP proxy | `[redaction.data_sources.<name>]` |
| Native database connection | A live PostgreSQL / MySQL / SQLite connection DuDuClaw opens itself | DuDuClaw's own `db_select` / `db_query` MCP tools — no external server involved | `[db_sources.<name>]` |

Both kinds are named the same way: a `db_field` rule's `source` field, and the dashboard's single "資料來源" (data sources) card, which lists both.

---

## Layer 1: the data-source registry

`[redaction.data_sources.<name>]` describes three facts a `db_field` rule needs and cannot carry on its own — which tools return a table's rows, how the table is decided for a given call, and where the records sit in the JSON:

```toml
[redaction.data_sources.crm_pg]
label = "客戶 CRM 資料庫"
tools = ["pg_query", "pg_select"]          # ≥1, exact name or trailing-* glob
table_arg = "table"                        # XOR a fixed `table = "customers"`
record_paths = ["$.rows[*]", "$[*]", "$"]  # this is already the default
key_alias = { name = "customer_name" }     # only when the tool renames columns
```

A `db_field` rule then names it:

```toml
[redaction.rules.crm_customers]
type = "db_field"
source = "crm_pg"          # `connector` still works as a deprecated alias
fields = ["customers.name", "customers.address", "orders.*"]
category = "DB_FIELD"
```

Two sources ship built in and cannot be redefined: `odoo` (the original `ODOO_TOOLS` table, now expressed as registry bindings — nine tools, mixed fixed/dynamic tables, per-tool key aliases for the ones that go through a mapper struct) and `duduclaw_db` (binds `db_select`, described below). Omitting both `source` and `connector` still defaults to `odoo`, so every pre-registry config keeps its meaning unchanged.

A table name no longer has to be dotted — `db_field` validation was relaxed from Odoo's `model.model` shape to also accept a plain SQL identifier (`customers`, `public.customers`) — but everything else about `db_field` fail-closed behaviour is unchanged: an unknown `source`, an empty `tools` list, a malformed `record_paths` entry, or a `table.column` string that doesn't match the pattern all refuse to load rather than skip silently.

---

## Layer 2a: the MCP proxy — redacting servers you didn't write

`duduclaw mcp-proxy --server <name> -- <cmd> [args…]` is a hidden internal subcommand: a stdio JSON-RPC pass-through that sits between the Claude CLI and one external MCP server. When an agent's turn has redaction active, the gateway rewrites that agent's per-spawn `.mcp.json` so every non-`duduclaw` **stdio** server launches through this proxy instead of directly — covering both a channel reply's Claude-CLI spawn (fresh spawn and the one-shot PTY fallback, both in `channel_reply.rs`) and a dispatch / cron / heartbeat / goal-loop turn's spawn (`claude_runner.rs`'s `prepare_claude_cmd`, via the same shared `mcp_proxy_cli_args` helper). The proxy applies the exact two operations the built-in choke point applies:

- a `tools/call` request's `arguments` go through the same egress decision (`Deny` answers right there, in JSON-RPC, and the call never reaches upstream; `Allow` may restore `<REDACT:…>` tokens back to real values for a whitelisted tool);
- the matching response's `result` goes through the same `redact_value` pipeline, with tool names namespaced `<server>.<tool>` so `match_tool = "crm_pg.pg_select"` can't accidentally catch a same-named DuDuClaw tool.

Everything else — `initialize`, `tools/list`, notifications, upstream-initiated requests, and any line that isn't parseable JSON — is forwarded byte for byte. Credentials in the wrapped server's original `env` block travel through the environment variable `DUDUCLAW_MCP_PROXY_ENV` (a JSON blob), not through `argv`, because `/proc/<pid>/cmdline` is world-readable on Linux while `/proc/<pid>/environ` is not. Fail-closed applies here too: the proxy runs the identical `McpRedactionLayer::try_init` three-way outcome as `duduclaw mcp-server` — redaction disabled means pure pass-through, misconfigured means the proxy refuses to start rather than forward anything unredacted.

**What this does not cover yet:**

- **HTTP/SSE MCP servers.** There's no child process to wrap, so the rewrite leaves them untouched and logs a warning — their tool results reach the model unredacted.
- **The PTY session pool** (`[runtime] pty_pool_enabled`, default off, documented as a standby path). A pooled REPL session outlives any single spawn call, so the per-spawn temp `--mcp-config` file this rewrite relies on has nowhere to attach — it would need a session-owned guard instead of a per-call one. Deferred, not built. The default fresh-spawn and one-shot-PTY paths above are unaffected.
- **codex / gemini / antigravity.** Their own MCP registration is not routed through the proxy at all yet.

---

## Layer 2b: the direct-API tool loop (openai-compat)

The CLI backends get redaction from `duduclaw mcp-server` or `duduclaw mcp-proxy`; a model driven through `duduclaw-llm`'s `run_tool_loop` (the openai-compat runtime — API-mode Grok/DeepSeek/MiniMax agents, for example) talks to the in-process `ToolRegistry` directly, with no subprocess to intercept. `duduclaw-llm` gained a `ToolInterceptor` trait for exactly this: `before_call(server, tool, args)` can deny or rewrite arguments before dispatch, `after_call(server, tool, args, &mut result)` can mutate the result the model sees afterward. The gateway's `RedactionToolInterceptor` implements it against the same `RedactionManager` the rest of the pipeline uses, so a rule written once behaves identically whether the model reached the tool through `duduclaw mcp-server`, the MCP proxy, or this in-process path. One more tool-dispatch path neither mechanism reaches yet: the local-inference tool loop (`local_llm.rs`), for a locally-hosted model calling tools — its results are not redacted either.

---

## The native connector (`duduclaw-db`)

For a customer with no MCP server at all — just a PostgreSQL / MySQL / SQLite database sitting there — the `duduclaw-db` crate is a first-party, read-only SQL connector with its own four MCP tools. Because it dispatches through DuDuClaw's own MCP server, its results reach the redaction choke point directly, with no proxy hop needed.

### Read-only by three independent layers

1. **Statement guard** — cheapest, least trusted alone: a single statement, must start with `SELECT` or `WITH`, no `;` outside a string literal. A data-modifying CTE (`WITH x AS (DELETE … RETURNING *) SELECT * FROM x`) passes this layer on purpose — it *is* a single `WITH` statement — because layer 2 is what actually stops it.
2. **Driver-level read-only** — PostgreSQL runs `BEGIN READ ONLY`, MySQL runs `START TRANSACTION READ ONLY`, and SQLite is opened with a read-only file handle. This is the layer that makes a write impossible, not the parser above it.
3. **Caps** — `max_rows` (default 200, hard ceiling 1000) and `timeout_ms` (default 10s, floor 100ms, ceiling 120s) bound what a single call can cost.

### Configuring a source

```toml
[db_sources.crm_pg]
label = "客戶 CRM 資料庫"
driver = "postgres"                       # postgres | mysql | sqlite
url = "secret://env/CRM_PG_DSN"           # or url_enc = "<ciphertext>"
allowed_tables = ["customers", "orders"]  # required, non-empty; ["*"] = everything
max_rows = 200
timeout_ms = 10000
```

`url` goes through the project's `secret://<backend>/<name>` credential doctrine — a plaintext connection string is refused at load unless the driver is `sqlite` (where the value is a filesystem path, not a credential, since a DSN with a password has no business sitting in `config.toml`).

`allowed_tables = ["*"]` is accepted but is the one setting that also unlocks `db_query` (below) — a real table list means "reachable, but only through `db_select`, where the table is a validated, bound identifier"; free-form SQL cannot be checked against a table allowlist without a real SQL parser, so it is refused entirely on any source that has one.

### Granting an agent access

Deny by default, same doctrine as every other capability. Hand-editing `agent.toml` is no longer the only way in: grant it from the dashboard's data-source wizard (step 4) or the grant dialog on a database row, from that agent's own settings page under Tools & permissions ("Databases this AI employee can use"), or by telling the agent in chat — the MCP `agent_update` tool understands `db_sources` / `db_sources_add` / `db_sources_remove`. All three routes land in the same place:

```toml
# agent.toml
[capabilities]
db_sources = ["crm_pg"]
```

The MCP dispatch choke point re-reads this on every call, so a grant made through any route takes effect immediately — visible on the very next chat turn, no gateway restart needed.

Two gates, both required: `Scope::DbRead` (`db:read` — the twenty-third MCP scope) plus this non-empty grant list are checked once at the MCP dispatch choke point before any tool handler runs; the specific `source` name is then checked again inside the handler, so a grant of `["crm"]` cannot reach a `payroll` source that happens to share the same driver.

### The four tools

| Tool | Args | Returns |
|---|---|---|
| `db_sources` | — | `[{name, label, driver}]` — only sources this agent is granted **and** that actually load; a granted-but-broken source is reported with its reason rather than silently omitted |
| `db_tables` | `source` | `{tables: [{name, columns: [{name, type}]}]}`, filtered to `allowed_tables` |
| `db_select` | `source`, `table`, `columns?`, `filter?` (`[{column, op, value}]`, `op` one of `= != < <= > >= like in`), `order_by?`, `limit?` | `{rows, row_count, truncated}` — identifiers validated and quoted, every value bound as a parameter |
| `db_query` | `source`, `sql`, `limit?` | Same shape. **Only available when the source's `allowed_tables` is exactly `["*"]`** — refused before a connection even opens otherwise |

Row values map deliberately, not by whatever the driver happens to decode: booleans stay booleans; integers and floats become JSON numbers; `NUMERIC`/`DECIMAL` becomes a **string** (a float would round money); timestamps become ISO-8601 strings; `bytea`/`BLOB` becomes base64; JSON columns come back parsed rather than double-encoded. A type nothing here knows how to decode becomes the honest placeholder `"<unsupported type: NAME>"` rather than being guessed at or silently dropped — a dropped key would make a redaction rule targeting that column match nothing without anyone knowing.

A pool is opened fresh per call rather than cached, so a rotated credential or an edited `[db_sources.…]` block takes effect on the very next call, not after a restart.

### The built-in `duduclaw_db` registry source

`duduclaw_db` binds one tool, `db_select`, with its table read from the call's own `table` argument, records at `$.rows[*]`, and column names passed through verbatim. This means `db_field` rules against **any** `[db_sources.*]` connection can reuse this one built-in source — `source = "duduclaw_db", fields = ["customers.name"]` — without registering anything else, because the binding matches on the `table` argument alone, not on which `db_sources` connection made the call. If two different connections both happen to expose a table literally named `customers` and you need to mask one but not the other, write an explicit `json_path` rule instead, with `match_args = { source = "crm_pg", table = "customers" }` (both are top-level `db_select` arguments, so either or both can gate a match). `db_query` is deliberately left unbound in the registry: a raw SQL result's columns come from whatever the statement projects, and "which table does this row belong to" isn't knowable from the call without a real parser.

---

## Local files (CSV / Excel / text)

A `db_field` rule can mask a column in Postgres or Odoo because both routes end at an MCP tool call — the one place redaction can see a key name. A file on an agent's own disk has no such choke point by default: the Claude CLI's built-in `Read` (and `head`/`cat` via `Bash`) are not MCP tools, so a `customers.csv` opened that way lands in the model's context whole, unseen by any rule. A channel attachment makes the gap worse, not better — `format_attachment_ref` only appends the saved file's path to the message text, so the agent still reaches for `Read` to open it. `office_script` and `sheets_read` already pass through the choke point, but only carry pattern rules — there's no column concept there either. And a PostToolUse hook can't rewrite a tool result after the fact, so "mask it on the way out" was never an option; the only way in is to make the read itself an MCP call, then block the built-in route back to it.

### Three tools, one path fence

| Tool | Args | Returns | Cap |
|---|---|---|---|
| `file_read` | `path`, `max_bytes?` | `{path, table, text, truncated}` | 512 KiB, plain text only |
| `csv_read` | `path`, `delimiter?`, `has_header?` (default `true`), `limit?` (default 200), `offset?` | `{path, table, columns, rows, row_count, truncated}` | 64 MiB on disk, `limit` capped at 2000 |
| `xlsx_read` | `path`, `sheet?`, `limit?`, `offset?` | same shape, plus `sheet` and `sheets` | 32 MiB on disk (a spreadsheet parser expands far past its on-disk size, so this cap is tighter than CSV's) |

`table` is always the file's own basename with its extension — `customers.csv`, not `customers`. Every path is resolved through one fence before a byte is read: canonicalize, then require containment inside the agent's own directory, `<agent_dir>/attachments`, `<home>/attachments`, or an operator-declared `config.toml [files] allowed_roots` entry. A `..` segment or a symlink escaping the fence is refused before canonicalization even runs, so the error names the real problem instead of a confusing "outside the roots". There's no per-agent capability gate on top of the fence (unlike `db_sources`) — the fence already answers "whose data is this" — so all three tools are always listed in `tools/list`, gated only by the `files:read` scope (`Scope::FilesRead`, the 24th MCP scope).

`csv_read` reads with the `csv` crate already in the workspace; a missing header row falls back to `c1..cN` column names. `xlsx_read` reads xlsx/xlsm/xls/ods with `calamine` (a new dependency, pure Rust, `dates` feature only); a cell maps to a JSON number, bool, null (empty), ISO-8601 string (date/time), or string, in that order. Neither tool's audit record carries cell content — only `path` / `table` / `row_count`, the shape of the read, the same rule `mcp_db.rs` follows for filter values.

### The built-in `duduclaw_files` source

A registry source ships already bound to the two structured readers: `table` comes from the tool's own **result**, not an argument (`table_result = "/table"`), records sit at `$.rows[*]`, and `free_form_names = true` — a file's table and column names don't have to be identifiers. That flag is what makes rules like this legal:

```toml
[redaction.rules.customer_files]
type = "db_field"
source = "duduclaw_files"
fields = ["customers.csv.name", "客戶清單.xlsx.地址"]
category = "DB_FIELD"
```

Each entry splits on its *last* dot — `customers.csv.name` is table `customers.csv`, column `name`. Because the table is the filename with no sheet in it, a rule on `客戶清單.xlsx.地址` fires no matter which worksheet `xlsx_read` happened to open — there's nothing sheet-specific to match against, so it covers all sheets of that workbook. A CJK column name compiles to the quoted path form under the hood (`$.rows[*]['地址']`) — the JSONPath grammar's `['key']` segment accepts any Unicode character except a quote or a newline, added precisely for this case; the plain `.key` form stays ASCII-only. `file_read` isn't bound to anything — it returns unstructured text, so only the pattern-matching passes apply to what it reads, never a column rule.

### The data-file guard

`[redaction] data_file_guard = "on" | "read_only" | "off"`, default `on`, takes effect only while redaction is actually active for the agent making the call. It's a PreToolUse hook (`data-file-guard.sh`, installed alongside the existing security hooks) that reads `DUDUCLAW_DATA_FILE_GUARD` — an env var the gateway sets at spawn time, never a config value the agent itself could read or change. `on` blocks the built-in `Read` on a `.csv/.tsv/.xlsx/.xlsm/.xls/.ods` path and blocks `Bash` whenever its command text names one; `read_only` blocks only `Read`; `off` blocks nothing. A block returns a deny with the message "此檔案受去識別化保護，請改用 csv_read／xlsx_read／file_read".

Two limits are stated plainly rather than glossed over. The `Bash` check is a filename heuristic — a command that assembles its path dynamically (`python -c "open(chr(99)+...)"`) walks straight past it. And the hook is a shell script, unlike its sibling `agent-file-guard`, which is a Rust subcommand specifically so it runs on Windows — on a Windows host with no `bash` on `PATH` the hook command itself fails, and Claude Code treats a non-2 exit as allow, so the guard is simply absent there. Neither gap is a bug waiting to be patched so much as a property of what a PreToolUse hook can promise: it lowers the odds of the model wandering onto the unredacted route by accident. The real protection is the MCP tool surface itself — a rule bound to `duduclaw_files` sees a value only once it's already inside `$.rows[*]`, regardless of whether the guard caught anything upstream.

### The attachment hint

`format_attachment_ref` — the line the gateway appends to a channel attachment reference in the message text — now appends one more sentence for csv/tsv/xlsx/xls/ods/txt/md/json: "請用 csv_read／xlsx_read／file_read 讀取". Every other extension (image, audio, video) stays byte-identical to before. It's a nudge, not an enforcement layer — the guard above is what actually stops the wrong tool from firing.

### Why `file_read` refuses spreadsheets

`file_read` returns one text blob; a `db_field` rule against `duduclaw_files` binds to `$.rows[*].<column>` inside a *structured* result. Feed a spreadsheet's raw bytes through `file_read` and a column rule like `客戶清單.xlsx.地址` would never find a `.rows[*]` to match against — it would sail through in the clear, protected by pattern rules alone. So `file_read` checks the extension first and refuses csv/tsv/xlsx/xlsm/xls/ods outright, redirecting the caller to `csv_read` / `xlsx_read` instead of silently returning an unprotected read.

---

## Custom rules: your own identifiers, no regex required

A data source tells redaction where a field *is*. Custom rules tell it what a value *looks like* — for the identifiers that only exist inside your company and that no built-in profile could possibly know: an employee number, an internal project codename, a customer code, a contract prefix.

Before 2026-09 the five built-in profiles (`general` / `taiwan_strict` / `taiwan_minimal` / `financial` / `developer`) were a fixed list you could tick but not extend. A custom profile file was technically resolved at boot, but nothing in the product could create one — you hand-wrote TOML or you had no custom rules.

### One rule = a data-type name + a way to recognise it

Each rule carries a **data-type name** you type yourself (`員工編號`, `Employee ID`, anything up to 32 characters) and exactly one matcher:

- **Keyword list** — the zero-syntax route. Paste the terms, one per line; each must be at least two characters. Matching is whole-word for ASCII-edged terms and substring for CJK, case-insensitive either way (the same semantics the `keyword` rule kind has always had).
- **Pattern** — a regular expression, for a shape rather than a list.

Every rule also has an **enabled** flag, so a rule can be parked without being deleted. `enabled` is a new field on the rule spec itself and applies to *every* rule kind — a `regex`, `keyword`, `identity`, `json_path` or `db_field` rule with `enabled = false` is skipped at engine compile time, not filtered at match time. Absent means `true`, so nothing you already have changes.

### Where they live

Custom rules are a **profile file**, not inline `[redaction.rules.*]` entries — the operator's mental model is "one more rule set", and profile files already show up in the profile list you tick:

```toml
# ~/.duduclaw/redaction/profiles/custom.toml
[meta]
name = "我的規則"
description = "在儀表板建立的自訂規則"
version = "1"

[meta.labels]                        # category id → what a human sees
CUSTOM_EMPLOYEE_ID = "員工編號"
CUSTOM_01 = "內部專案代號"

[rules.employee_id]
type = "regex"
pattern = 'EMP-\d{4}-\d{4}'
category = "CUSTOM_EMPLOYEE_ID"
priority = 60
enabled = true
```

Two things are worth spelling out.

**`[meta.labels]` is new.** A token category is machine-shaped by construction (`[A-Z0-9_]{1,32}`), so a profile that invents its own categories needs somewhere to say what they mean. An ASCII data-type name becomes `CUSTOM_<SLUG>`; a name with no ASCII letters — `內部專案代號` — becomes `CUSTOM_NN` with the next free two-digit counter, and the readable name is recorded here. `redaction.get` returns the merged map across every currently-listed profile as `category_labels`; the dashboard resolves a category by checking that map first, then its own translations, then falling back to the raw id.

**Priority 60** puts a custom rule above the default band (50) — your own employee-id rule beats a generic digit-run rule — and below the precise built-ins (100), so a national-ID pattern still wins an overlap.

Writes go through an advisory file lock and a temp-file-plus-atomic-rename, the profile name is added to `config.toml [redaction] profiles` if it isn't already there, and the live pipeline is rebuilt immediately — the same hot reload `redaction.update` uses, no gateway restart.

### Generating a pattern from examples

If you can't write a regex, paste 2–5 real values instead (`EMP-2024-0133`, `EMP-2025-0007`) plus up to 3 counter-examples — things that look similar but must *not* be masked — and `redaction.suggest_pattern` writes the pattern for you.

Three engines are tried in order, and the response says which one answered:

1. **Local inference**, if a local backend is actually reachable.
2. **The cloud utility model**, through the account rotator.
3. **A heuristic** with no model at all: split each example into runs of digits / upper-case / lower-case letters and literal separators, require the examples to share that shape, and emit `\d{4}`, `[A-Z]{2,4}`, escaped separators. If the strict pass can't align the examples, a looser pass treats every alphanumeric run as `[A-Za-z0-9]` and tries again.

Whatever produced it, the pattern **must pass verification before it is returned**: every example has to match it in full (anchored), and no counter-example may match it anywhere. The counter-example check is deliberately unanchored — the live engine searches rather than anchoring, so a pattern that fires *inside* a counter-example would still redact it. A model whose first answer fails verification gets exactly one retry, carrying the failed checks as feedback; if it fails again the chain falls through to the next engine. When no engine produces a pattern that survives verification, the answer is `pattern: null` and `all_ok: false` — an honest empty result, never an invented one.

The example values you paste go into the prompt and nowhere else: they are never written to a log or to the audit trail. Inside the prompt they are wrapped in XML delimiters and explicitly labelled as data rather than instructions. The RPC is rate-limited to 10 calls per minute per operator.

### Importing a rule pack

`redaction.profiles.import` takes a TOML rule pack (pasted or uploaded) and lands it as a second custom profile at `~/.duduclaw/redaction/profiles/<slug>.toml` — the route for an integrator deploying the same rules to many customers.

The on-disk slug is `[a-z0-9_-]{1,40}` and is resolved in three steps: an explicit `name` parameter; an ASCII slug of `[meta] name` when that is free; otherwise `pack_<first 8 hex of SHA-256 of the normalised `[meta] name`>`. That last step is what a Taiwanese pack normally takes — `製造業客戶包` has no ASCII to slug — and it is deterministic, so re-importing the same pack overwrites the profile it created last time instead of piling up a second copy. The human-readable `[meta] name` is untouched by any of this: it stays the profile's label everywhere the dashboard shows it, and only the file name is machine-shaped. An **explicit** `name` that is invalid or collides with a built-in is an error (you typed it, so you can fix it); an auto-derived one never dead-ends, because the import dialog has no name field to refuse to.

Rules are validated one at a time, and a bad rule is **skipped with a reason and the line its `[rules.<id>]` header sits on** rather than taking the whole import down: a pattern that doesn't compile, a keyword shorter than two characters, an illegal rule id, an illegal category. Rule kinds the card doesn't own (`json_path`, `db_field`, `identity`) are proven by actually compiling them against the live engine options, so a rule that would poison the next reload is caught here instead of landing on disk. A pack with **zero** usable rules is an error, not an empty profile — listing a rule set in the dashboard that redacts nothing would be worse than refusing.

Pass `dry_run: true` to get the whole report — name, imported count, per-rule skip reasons, the categories covered — without writing anything.

`redaction.profiles.remove` deletes a custom profile file, drops it from `[redaction] profiles` and reloads. Built-ins are refused: they're compiled into the binary, there is no file to delete, and silently unlisting one would look like a delete that worked.

### Fail-closed reading

If `custom.toml` exists but cannot be read or parsed, `redaction.custom_rules.list` returns an **error**. It never degrades to an empty list. An operator looking at "you have no rules" would conclude their rules were deleted, when in fact they are still on disk and the whole pipeline is poisoned by the same parse failure.

### Trying a rule before you save it

The wizard's last step runs the rule you just built against a sample — **before** anything is written. `redaction.dry_run` grew two optional parameters for that:

- **`sample_text`** — plain prose instead of `sample_json`. It's wrapped as a JSON string value internally (exactly `JSON.stringify(text)`), so the sample still travels the same path through the pipeline as a real tool result. `sample_json` wins if both are given.
- **`draft_rules`** — an array of `{ id, label?, category, kind, keywords?, pattern? }`, validated by the very same code that validates an upsert (so a preview can never pass something the save would reject). They are compiled into a candidate rule set **for that one call**, layered on top of the live config's inline rules so a draft sharing an id with an existing rule shadows it — which is what previewing an *edit* means. Nothing is written anywhere: the candidate pipeline is dropped when the call returns, and the saved rule set is untouched.

Hits produced by a draft carry the draft's own `id` as `rule_id`, so the UI can count "this rule · N hits". Everything else — built-in profiles, field rules — reports exactly as it did before. An invalid draft is an error for the whole call, never a partial run: a preview that quietly dropped a rule would report coverage the saved rule set is not going to deliver.

### RPCs

| Method | Params | Returns |
|---|---|---|
| `redaction.custom_rules.list` | — | `{ rules: [{ id, category, label, kind, keywords, pattern, example, enabled }] }` |
| `redaction.custom_rules.upsert` | `{ id?, label, category?, kind, keywords?, pattern?, enabled? }` | the saved row, plus `applied` / `warning` from the hot reload |
| `redaction.custom_rules.remove` | `{ id }` | `{ ok, removed, applied, warning }` |
| `redaction.custom_rules.set_enabled` | `{ id, enabled }` | the updated row |
| `redaction.suggest_pattern` | `{ examples: [2..5], counter_examples?: [0..3] }` | `{ pattern, engine, checks: [{ value, kind, matched, ok }], all_ok }` |
| `redaction.profiles.import` | `{ toml, name?, dry_run? }` | `{ name, imported, skipped: [{ rule_id, line?, reason }], categories, dry_run }` |
| `redaction.profiles.remove` | `{ name }` | `{ ok, removed, applied, warning }` |
| `redaction.dry_run` *(extended)* | `{ sample_json? \| sample_text?, tool?, args?, draft_rules? }` | `{ hits: [{ pointer, rule_id, category, token }], token_count, restored_ok }` |

All seven sit behind the same admin gate as the rest of `redaction.*`: every one of them edits the rule set that decides what leaves the deployment.

The **example** field on a list row deserves a note. For a keyword rule it is the first keyword — a real value you typed and will recognise. For a pattern rule it is **synthesised from the pattern**, not remembered from the form: `EMP-\d{4}-\d{4}` renders as `EMP-0000-0000` (literals verbatim, a character class contributes one representative character, a repetition contributes its minimum count or 4 when unbounded, an alternation its first branch, anchors nothing). The values you pasted into the wizard are deliberately not stored anywhere, so there is nothing to show but a reconstruction. When synthesis declines — a pattern that would expand past 128 characters, or one that doesn't parse — the row shows the pattern itself, which is honest rather than wrong.

---

## AI detection (OpenAI Privacy Filter)

Every rule kind above matches a *shape*: a national ID, a credit-card number, an API key, a column you named. Names, street addresses, birthdays and account numbers have no shape. Before this, the only way to catch them was to know the exact database column they lived in — which works for your own ERP and does nothing for a pasted email thread, an uploaded spreadsheet with an unexpected column, or a chat message.

The `ai_pii` profile ("AI 智慧偵測") closes that. It runs [OpenAI's Privacy Filter](https://huggingface.co/openai/privacy-filter) — a 1.5B-parameter (50M active) bidirectional token classifier, Apache-2.0 — **locally**, through ONNX Runtime. Nothing is sent anywhere: no API, no telemetry, no network at inference time.

### What it detects

Eight labels, mapped onto redaction categories so a model hit and a regex hit of the same kind tokenise identically (an `EMAIL` found by the model and one found by `general`'s regex are the same category, so a source filter naming `EMAIL` covers both):

| Model label | Category | |
|---|---|---|
| `private_person` | `PERSON` | names |
| `private_address` | `ADDRESS` | street addresses |
| `private_email` | `EMAIL` | |
| `private_phone` | `PHONE` | |
| `private_url` | `URL` | |
| `private_date` | `DATE` | birthdays and other personal dates |
| `account_number` | `ACCOUNT_NUMBER` | bank / customer account numbers |
| `secret` | `SECRET` | passwords, keys, tokens |

### What it is not

**It is not an anonymisation guarantee, and it will miss things.** The model card calls it a data-minimisation component, not a compliance control, and our own measurements agree. On 25 zh-TW sentences carrying 119 spans:

| | recall |
|---|---|
| overall | **79.8%** (89.1% if you don't require the right label) |
| phone, email | 100% |
| address | 88% |
| URL | 83% |
| **person names** | **72%** (76% ignoring label) |
| date | 58% |
| account number | 60% (100% ignoring label — Taiwanese bank accounts are frequently labelled `private_phone`; both are redacted, so the data is still masked) |
| long mixed-language document (3K chars) | 88.0% / 95.1%, zero false positives |

False positives ran at 5.5% (6 of 110), all of them a span reaching backwards to swallow a Chinese field label such as `出生日期` — over-masking, not a leak. Two other known behaviours, both visible in our tests: a name immediately followed by a date can merge into one span (the date is still covered, just labelled `private_person`), and an email span can include the leading space.

Independent benchmarks on harder corpora (web crawl, medical records, legal documents) report recall between 10% and 38%. **The gap is almost entirely recall**, which is exactly why this profile is priority 30 — below every pattern rule — and why you should keep `general` / `taiwan_strict` switched on alongside it. It is a second layer, not a replacement.

### Installing the model

The release binary carries neither the model nor ONNX Runtime. The first time you tick 「AI 智慧偵測」 the card offers a download:

| artefact | size | source |
|---|---|---|
| model (6 files) | ~945 MB, dominated by `onnx/model_q4.onnx_data` | Hugging Face `openai/privacy-filter`, pinned to one commit |
| ONNX Runtime | 7–75 MB depending on platform | Microsoft's official GitHub release for the version `ort` targets (1.24.2) |

Every file is pinned with its URL, byte length and SHA-256 in the binary. Downloads stream to `<name>.part`, resume with an HTTP `Range` header, are hashed, and only then renamed into place — so a file at its final path is always a verified file. A hash mismatch deletes the partial download and fails the whole install; there is no "most of the model". An `installed.json` marker is written last and names the revision, so a partial install reads as **not installed** rather than as a broken one.

```
~/.duduclaw/models/privacy-filter/     config.json, tokenizer.json, tokenizer_config.json,
                                       viterbi_calibration.json, onnx/model_q4.onnx(+_data),
                                       installed.json
~/.duduclaw/lib/onnxruntime/1.24.2/    libonnxruntime.dylib | libonnxruntime.so | onnxruntime.dll
```

**Platform support.** macOS (Apple Silicon), Linux (x86-64 and arm64), Windows (x64 and arm64). **macOS on Intel is not supported**: Microsoft publishes no `osx-x86_64` build at ONNX Runtime 1.24, so there is nothing honest to install. The card says so, and the rule fails closed rather than pretending.

### Performance and memory

Measured on a 10-core Apple Silicon machine, ONNX Runtime CPU, 4 intra-op threads, the `q4` graph:

- **63–161 ms** for a typical zh-TW sentence; ~214 ms per 1,000 characters on long documents.
- **~1.1–1.7 GB** resident while loaded. The session unloads after `idle_unload_minutes` (default 10) with no inference and reloads on the next one, so an idle deployment gets the memory back.
- If the machine is short on RAM and the weights get paged out, a single sentence can take **seconds** instead of milliseconds. Give it headroom or leave the profile off.

Results are cached by text (256 entries, LRU), which matters more than it sounds: one `ner` rule compiles into one rule *per label*, and the eight of them share the cache, so a turn costs one inference rather than eight.

### Configuring

Tick 「AI 智慧偵測」 in 偵測規則集, or add the profile by hand:

```toml
[redaction]
profiles = ["taiwan_strict", "ai_pii"]     # keep the pattern rules on

[redaction.ner]                            # every key optional
threads = 4                 # ONNX Runtime intra-op threads
idle_unload_minutes = 10    # 0 disables unloading
min_chars = 24              # shorter text never reaches the model
max_chars = 32000           # longer text is chunked on paragraph boundaries
cache_entries = 256
# model_dir = "/some/other/place"          # default: <home>/models/privacy-filter
```

A rule of your own can narrow the labels:

```toml
[redaction.rules.names_only]
type = "ner"
category = "PII"            # unused: each match is categorised by its label
labels = ["private_person", "private_address"]
priority = 30
```

`min_chars` and `max_chars` can be overridden per rule. An unknown label is a load error, not a skipped rule.

### Fail-closed behaviour

Consistent with the rest of the pipeline: **a rule that cannot work must not look like one that does.**

- Ticking the profile with no model installed makes the `ner` rule fail to compile, which fails the whole `RedactionManager` — the gateway enters the [poison state](#the-dashboard) and `duduclaw mcp-server` refuses to start rather than serving tool results under a rule set that isn't running. The card warns before you save.
- The same happens on an unsupported platform, or in a build compiled without the `ner` feature. The rule kind still *parses* everywhere, so a profile stays portable across builds; it just refuses to compile where it cannot run.
- Removing the model (`redaction.model.remove`) rebuilds the pipeline immediately, so the state is visible rather than latent until the next restart.

### Auditing model hits

`AuditEvent::Redact` now records which engine found each span:

```json
{"ts":"2026-09-24T…","event":"redact","rule_id":"ai_pii","category":"PERSON",
 "token":"<REDACT:PERSON:…>","engine":"ner",
 "model_revision":"7ffa9a043d54d1be65afb281eddf0ffbe629385b"}
```

`engine` is `"rule"` for every deterministic matcher and `"ner"` for the model; `model_revision` is present only for model hits. Audit lines written before this release have no `engine` field and read back as `"rule"`.

### RPCs

| Method | Params | Returns |
|---|---|---|
| `redaction.model.status` | — | `{ installed, model_revision, ort_version, size_bytes, state, progress, error, avg_latency_ms, p50_latency_ms, calls, last_used_at, platform_supported, reason }` |
| `redaction.model.install` | — | `{ started, installed }` — idempotent; a second call while downloading returns `started: false` |
| `redaction.model.cancel` | — | `{ cancelled }` — partial downloads are kept so the next install resumes |
| `redaction.model.remove` | — | `{ removed }` — deletes the model, keeps the ONNX Runtime library |

`state` is one of `absent` / `downloading` / `ready` / `loaded` / `error`. `avg_latency_ms` and `p50_latency_ms` come from a rolling window of the last 100 real inferences and are `null` before the first one — never an estimate. `available_profiles[]` on `redaction.get` carries `requires_model: true` for any profile holding a `ner` rule, so an imported rule pack that uses one gets the same download prompt as `ai_pii`.

All four sit behind the same admin gate as the rest of `redaction.*`.

---

## The dashboard

**設定 → 去識別化** now shows the previously separate "外部系統" and "資料來源" cards merged into one, "外部系統與資料來源":

- **資料表欄位規則** card — unchanged in shape: one card per rule (id, category, kind badge, a one-line coverage summary). *新增* toggles between two modes: **資料表欄位（簡易）** — a `source` dropdown, a multi-value field list (`*` supported, with a note that it keeps `id`), category, and who may restore; and **JSON 路徑（進階）** — `match_tool`, `match_args`, `paths`, `exclude_keys` for anything the simple form can't express. The dropdown now shows display names only — Odoo ERP, 地端檔案, and each system you've defined; the three built-in registry ids (`odoo` / `duduclaw_db` / `duduclaw_files`) never appear as raw values. A source with a live database connection still turns the 資料表 field into a table/column picker backed by `db_sources.tables`. Every save dry-compiles the whole resolved rule set first; a bad path or an unknown source is shown inline and nothing is written. A **試跑** button posts a pasted JSON sample (plus a simulated tool name and arguments) through the real pipeline and shows a hit table: pointer, rule id, category, token; it never shows the original value.
- **外部系統與資料來源** card — replaces the earlier separate "外部系統" and "資料來源" cards; the 鼎新／Salesforce／HubSpot template presets are gone, covered instead by the generic "custom MCP tool" type. Rows appear in a fixed order: **Odoo ERP** (connection status from `odoo.status`; when not connected, the row shows only a link to the integrations page, where the actual connection is configured), one row per `db_sources` connection, one row per custom `data_sources` registry entry, then a fixed **地端檔案（CSV／Excel）** row that needs no setup at all. The three built-in registry names (`odoo` / `duduclaw_db` / `duduclaw_files`) never render as their own row: they are the plumbing the four visible row kinds resolve through. Every row has two columns, **讀進來** (a plain-language summary — Odoo's fixed table list, or a database connection's allowed tables) and **寫回去** (a three-way restore-policy dropdown plus an audit-reveal checkbox; database connections and local files are read-only and show "不適用"). A clickable "N 條欄位規則" badge jumps to the field-rules card, filtered to that system. A database-connection row also carries an "{count} AI employees can use this" badge (a warning-toned "No AI employees are authorized yet" at zero), which opens a small dialog with the same checkbox picker as wizard step 4. If an AI employee still references a source that no longer exists, the card shows a compact notice naming who and which ids — the fix happens on that employee's own settings page.

  Write-back keys are derived automatically and never shown to the operator. Odoo always writes to `odoo_*`. A custom tool source collapses to a single `<server>.*` glob when its `tools` share one `duduclaw mcp-proxy`-style prefix, or falls back to one exact key per tool; either way every derived key is written the identical rule, so it still reads as one policy. Any `tool_egress` entry left over from before this UI existed collects in a collapsible **其他寫回規則（進階）** section at the bottom, so a hand-edited leftover never silently disappears.

  **新增資料來源 wizard, four fixed steps.** Step 1, **類型**: database / Odoo / custom MCP tool (picking Odoo just shows a description and a link to the integrations page; 地端檔案 is description-only and not selectable, since it is the free row above). Step 2, **連線／工具**: only the fields that type needs — database asks for label, driver, connection string, and allowed tables; custom tool asks for label, tool list, and how the table is identified (from an argument, fixed, or from the result) — with `record_paths` / `table_result` / key aliases / timeouts folded behind an "進階設定" disclosure that already has working defaults. Step 3, **測試**: a database type actually connects and lists tables to check off; a custom-tool type pastes a sample and dry-runs it. This step can be skipped for a custom tool source (the button reads "跳過" instead of "下一步"), since a brand-new source usually has no rule referencing it yet. Step 4, **寫回**, is no longer a single "不適用" screen for a database connection — it now asks "Which AI employees can use this database" as a checkbox list, pre-ticked to the current grant when editing, archived employees sorted last and muted, with a note alongside that the connection itself is read-only. A custom tool source's step 4 is unchanged: restore policy plus the audit checkbox. The same checkbox list also lives on the AI employee's own settings page under Tools & permissions — either route takes effect immediately, and a ticked id no longer present in config is marked "no longer exists" there so the operator can untick it deliberately rather than have it silently dropped. Editing an existing source always opens on step 2; the type choice from step 1 can't be changed after creation. The internal identifier is derived from the label (a label with no ASCII characters gets a `source_xxxxxx` / `db_xxxxxx` code) and is shown read-only under 進階設定 for anyone writing TOML; the UI always uses the label.

  **Known limitations.** There is no persisted "已驗證" state for a custom source: the step-3 test result lives only while the dialog is open, and saving does not record that a connection was ever proven to work. A custom tool source's step-3 dry run reports 0 hits until at least one field rule actually references it; that is expected (no rule to match yet), not a connection failure. Every database connection resolves through the one generic `duduclaw_db` source, so the field-rules form adds a "選擇連線" sub-picker purely to decide which connection's table list to suggest; the saved rule itself carries no record of which connection was used.
- **Poison banner** — when the gateway's redaction manager failed to build from a parseable-but-broken `[redaction]` config (or the config itself doesn't parse), a destructive-tone banner sits above everything else in the card: the title "De-identification protection failed to start", the raw reason text underneath (shown verbatim, whatever language the underlying error was raised in), and a "since \<time\>" line noting it's recorded in the audit log and activity feed. The gateway keeps serving chat traffic (no tools, no exposure) but every MCP tool is unavailable until the config is fixed and saved, which clears the banner and hot-reloads the pipeline without a restart.

---

## Walkthrough: from zero to masking `customers.name` on PostgreSQL

1. **Register the connection.** Either fill in 外部系統與資料來源 → 新增資料來源 → 類型: 資料庫 in the dashboard (the wizard's step 3 tests the connection before you finish), or add it to `config.toml` directly:

   ```toml
   [db_sources.crm_pg]
   label = "客戶 CRM 資料庫"
   driver = "postgres"
   url = "secret://env/CRM_PG_DSN"
   allowed_tables = ["customers", "orders"]
   max_rows = 200
   timeout_ms = 10000
   ```

   Set `CRM_PG_DSN` in the gateway process's own environment to the real `postgres://user:pass@host/db` string — it never touches `config.toml` in plaintext.

2. **Grant the agent.** Either tick it in the checkbox list on step 4 of 外部系統與資料來源 → 新增資料來源 (pre-ticked to the current grant when editing an existing source), or just tell the agent "give me access to that database" — both land on the same id in that agent's `agent.toml`:

   ```toml
   [capabilities]
   db_sources = ["crm_pg"]
   ```

3. **Write the field rule**, reusing the built-in `duduclaw_db` registry source (no separate registry entry needed for the common case):

   ```toml
   [redaction.rules.crm_customer_names]
   type = "db_field"
   source = "duduclaw_db"
   fields = ["customers.name"]
   category = "DB_FIELD"
   restore_scope = { kind = "owner" }
   ```

   This expands at load into one `json_path` rule bound to `db_select`, gated on `args.table == "customers"`, masking `$.rows[*].name`.

4. **Prove it fires** — either paste a sample through the dashboard's 試跑 button (tool `db_select`, args `{"table": "customers"}`), or from a shell:

   ```bash
   duduclaw redaction verify sample.json --tool db_select --arg table=customers
   ```

   The report lists every hit's JSON pointer, rule id, masked-but-visible original (`王**`), token, category, and a round-trip restore check — the same evidence format used everywhere else in the redaction pipeline.

5. **Ask the agent a question that touches `customers`.** The `name` column now reaches the model as a token; the channel reply still shows the real name to the owner, restored at the trusted egress boundary like any other redacted field.

### Attachment CSV in three steps

A channel attachment needs no `db_sources` grant and no connection string — the file is already sitting on disk, inside the fence by construction.

1. **Write the rule**, reusing the built-in `duduclaw_files` source — no registry entry to create:

   ```toml
   [redaction.rules.attachment_customers]
   type = "db_field"
   source = "duduclaw_files"
   fields = ["customers.csv.name", "customers.csv.email"]
   category = "DB_FIELD"
   ```

2. **Send the file.** A user attaches `customers.csv` in a channel; the attachment line the agent sees carries the "請用 csv_read／xlsx_read／file_read 讀取" hint, and — if the guard is `on` — a `Read` attempt on that path is refused outright.
3. **The agent calls `csv_read`.** The `name` and `email` columns reach the model as tokens; the channel reply still shows the real values to the file's owner, restored at the same trusted egress boundary as any other redacted field.

---

## Boundaries

- **Four gaps remain, honestly enumerated.** HTTP/SSE MCP servers (no child process to wrap; a warning is logged). The PTY session pool (`[runtime] pty_pool_enabled`, default off, documented as standby) — it needs a session-owned rewrite guard instead of the per-call one this feature ships, and that isn't built yet. The codex / gemini / antigravity runtimes — their own MCP registration doesn't route through the proxy at all. And the local-inference tool loop (`local_llm.rs`) — a locally-hosted model's tool calls reach neither the proxy nor the `ToolInterceptor`. Channel replies (fresh spawn and one-shot PTY) and dispatch / cron / heartbeat / goal-loop turns are covered; these four are not.
- **The registry describes shape, not semantics.** A `record_paths` or `table_arg` typo doesn't quietly under-protect a column — a wrong entry either fails to load (fail-closed) or matches nothing, which `試跑` will show you as zero hits.
- **`db_query` only exists where an operator explicitly said `allowed_tables = ["*"]`.** There is no partial or best-effort enforcement of an allowlist against arbitrary SQL — the design refuses the whole tool rather than pretend to filter it.
- **A pool is opened per call**, not cached — the right trade for a tool an agent calls a handful of times per turn, and it means a credential rotation or a config edit takes effect immediately, with no gateway restart.
- **AI detection misses things, by measurement, not by hedging.** ~80% recall on zh-TW overall and ~72% on person names, worse on corpora that look nothing like ours. Keep the pattern profiles on; treat `ai_pii` as a second layer over them, never as the thing that makes a deployment compliant. It also costs ~1.1–1.7 GB of RAM while loaded and 60–160 ms per sentence, and it does not exist at all on Intel macOS.
- **The data-file guard is a nudge, not a sandbox.** Its `Bash` check is a filename heuristic — a command that builds its path dynamically (`python -c "open(chr(99)+...)"`) walks straight past it — and on a Windows host with no `bash` on `PATH` the hook simply doesn't run, since it's a shell script and Claude Code treats a failed hook command as allow. Neither limit is discovered later; both are stated in the hook's own source. What actually keeps a local file's column values inside redaction's reach is the MCP tool surface (`file_read` / `csv_read` / `xlsx_read`), not the guard sitting in front of it.
- **A `db_sources` grant takes effect immediately, with one exception.** The MCP dispatch choke point re-reads `agent.toml` on every call, and a fresh chat turn sees it right away because each turn spawns a new CLI process; the one exception is the default-off `[runtime] pty_pool_enabled` pooled REPL, whose tool list only picks up a grant change once that session is recycled.
- **`db_sources_remove` is deliberately not checked against config.** `db_sources` and `db_sources_add` both require the id to exist under `config.toml [db_sources.<id>]`; `db_sources_remove` does not, so a stale grant can still be revoked by name even after the operator has deleted the source it pointed at.

---

## The takeaway

`db_field` used to mean "Odoo, and only Odoo." It now means "any table this agent is allowed to see through any tool" — a customer's own MCP server (redacted at arm's length, through a proxy), or a database DuDuClaw connects to itself (redacted at the source, through its own MCP tools). Both routes land in the same registry, the same dashboard card, and the same `source =` field on a rule, so an operator writes the masking policy once and doesn't need to know or care which route the data actually took to get there.
