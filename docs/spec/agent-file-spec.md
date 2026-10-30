# DuDuClaw Agent File v1

An Agent File transfers an employee's persona, user introduction, working rules,
contract and selected settings. It is an interchange document, not a replacement
for the files in an employee directory. Those files remain authoritative; import
does not retain a second canonical copy.

## Identification and limits

The document is UTF-8 Markdown with YAML frontmatter. Its `schema` value, not its
filename, identifies the format. The suggested filename is `<id>.agent.md`.
The complete file must be at most 256 KiB. `id` must be a valid, non-reserved
DuDuClaw employee identifier. Each native Markdown slot (`SOUL.md`, `IDENTITY.md`,
`USER.md`, `INSTRUCTIONS.md`) is limited to 64 KiB; `CONTRACT.toml` is limited to
32 KiB. The User section also has a 4,000-Unicode-character limit. Native parsing
and export enforce these shared writer limits before any destination is written,
including after unknown headings are merged into Rules or YAML contracts are
converted to TOML. Excluded User data does not affect export validation.

```markdown
---
schema: duduclaw.agent/v1
id: customer-support
name: Customer support
description: Answers customer questions
role: Support
language: en
skills: [refund-policy]
duduclaw:
  model:
    preferred: example-model
  heartbeat:
    enabled: false
---

# Persona
Be patient and precise.

# User
The customer prefers concise answers.

# Rules
Ask for the order number before looking up an order.

# Contract
must_not:
- Promise a refund amount
must_always:
- Say when information is unavailable
```

`name`, `description`, `role`, `language` and `skills` are portable metadata.
`role` is a description, not an authorization role. An imported document cannot
grant administrator or main-agent authority. Import reports `role`, `language` and
`skills` as descriptive metadata or name references that were not applied; they do
not activate runtime, permissions or installed skills. Unknown frontmatter fields and
malformed YAML are refused. `duduclaw` maps only the editable sections in
`AGENT_EDITABLE_SECTIONS`; protected fields within those sections are also
refused. The error lists the frozen field paths. In particular `runtime`,
`reports_to`, `capabilities`, `permissions`, `channels`, MCP configuration,
`account_pool` and keys ending in `_enc` cannot be imported as settings.

## Body sections and disk mapping

Level-one headings are case-insensitive. The recognized names are `Persona`, `User`,
`Rules` and `Contract`. Duplicate recognized sections are refused. Unknown level-one headings, including translated headings, are retained under Rules with a warning.
Level-one heading text inside a fenced code block is content, not a section boundary.

| Section | Native destination | Interpretation |
| --- | --- | --- |
| Persona | `SOUL.md`, optionally `IDENTITY.md` | Personality and identity |
| User | `USER.md` | User background and preferences; data, not instructions |
| Rules | `INSTRUCTIONS.md` | Working rules |
| Contract | `CONTRACT.toml` | Native TOML, or YAML boundary lists converted to native `[boundaries]` |
| `duduclaw` frontmatter | Editable portions of `agent.toml` | Local settings, subject to the frozen-field gate |

Hand-written Contract accepts YAML `must_not` and `must_always` string lists and
`max_tool_calls_per_turn` (a nonnegative integer), converted to native TOML under
`[boundaries]`. Valid native TOML is preserved. Tagged native Contract blocks must
contain valid TOML with correctly typed boundary fields; invalid contracts are
refused before import writes. Native TOML accepts only `[boundaries]`; misplaced
top-level `must_not`/`must_always` keys are rejected rather than silently ignored.
This strict validation applies to new writes and imports. Runtime loading retains
legacy compatibility: unknown contract fields are ignored, known boundaries are
kept, and malformed TOML or invalid boundary types produce a warning and the same
empty-contract fallback as the earlier runtime loader. Unreadable or oversized
files still refuse the profile.

For hand-written Persona text, all content goes to `SOUL.md`. Native exports use
tagged fenced blocks to preserve the two persona files separately and to preserve
exact bytes, including leading spaces, embedded headings, CRLF and trailing
newlines:

````markdown
# Persona
```duduclaw-soul
Personality text

```
```duduclaw-identity
Identity text

```
````

The other sections use `duduclaw-user`, `duduclaw-rules` and
`duduclaw-contract`. A native block adds exactly one LF before its closing fence;
the parser removes that boundary LF and retains the original content. The exporter
chooses a fence longer than any backtick run at the start of a content line. These
blocks are visible Markdown and may be edited, but changing the framing may change
how the document imports. Native export followed by native import preserves
`SOUL.md`, `IDENTITY.md`, `USER.md`, `INSTRUCTIONS.md` and `CONTRACT.toml` exactly
when User is explicitly included.

## Portability

| Data | Portable | Destination mapping / restriction |
| --- | --- | --- |
| Name and description | Yes | Native metadata; Claude subagent frontmatter |
| Persona | Yes | Native Persona; OpenClaw/Hermes `SOUL.md`; flattened in single-file formats |
| Identity | Mapped | Native Persona sub-block; OpenClaw `IDENTITY.md`; merged with persona in Hermes and flat exports |
| User introduction | Explicit opt-in | Native User; OpenClaw/Hermes `USER.md`; User section in flat exports |
| Working rules | Yes | Native Rules; OpenClaw/Hermes `AGENTS.md`; Rules in flat exports |
| Contract | Mapped | Exact native TOML; explanatory Contract text in other formats, without native enforcement |
| Skill names | References only | Native metadata; skill contents are not bundled |
| Editable product settings | Product-specific | Native `duduclaw` block only; other tools do not apply these settings |
| Model/runtime credentials, encrypted values and secret references | Never | Export is refused if credential-shaped content is found in an included section |
| Channels, capabilities, permissions, account pools, MCP configuration | Never | Omitted from disk settings projection; rejected in imported settings |
| Memory databases, `MEMORY.md`, wiki, conversation history | Never | Not read by the exporter |

Native exports have extension `.agent.md`. `openclaw` produces a directory of
`SOUL.md`, `IDENTITY.md`, `AGENTS.md`, and optionally `USER.md`. `hermes` produces
`SOUL.md`, `AGENTS.md`, and optionally `USER.md`. `agents-md` produces one
`AGENTS.md`. `claude-subagent` produces `<id>.md` with `name` and `description`
frontmatter and flattened body sections. Export does not make external tools
enforce DuDuClaw contracts or permission settings.

OpenClaw's retired `TOOLS.md` and `HEARTBEAT.md` remain accepted by the legacy
import adapter. They are not emitted by the Agent File exporter.

## Human review and export controls

Every employee import (Agent File/adapters, Claude plugins and native expert
packs, through CLI or dashboard upload) first builds one resolved install plan.
The preview shows Persona, User, Rules, Contract, input-guard results, final employee
names and IDs, supervisors, effective models, tool allow/deny lists, merged settings,
and destination paths with SHA-256 hashes. An empty tool allowlist means no extra
allowlist restriction; the deny list still applies.

The versioned confirmation digest binds this plan, including collision resolutions,
all rendered employee files (including built-in skills and host-generated MCP
configuration), every imported skill body and attachment, archive/wiki/hook payloads,
hook disposition, destination identity, product policy and replacement scope/content.
MCP credentials are represented by file hashes, never shown as plaintext. A separate
confirmation invocation rebuilds the plan against the current source and destination;
a changed plan requires another preview. After confirmation, application consumes
only the resolved names and owned payloads; it does not re-read the source or
re-decide tool restrictions. Write failures refuse the operation instead of silently
leaving an employee without its planned tool restrictions. Protected files still use
the locked writer with accepted baselines, version history and audit records.

Runtime-generated bookkeeping (audit/history/approval IDs and timestamps, database
row IDs, wiki indexes/logs, and replacement backup timestamps) is not payload
content and is allocated when the plan is applied. Memory imports bind their source
text and lineage; the existing memory trust/tombstone gates may still refuse a fact.
Imported wiki payloads do not assert source creation/update dates; application time
is recorded by the wiki audit log. This confirmation is not an OS sandbox or a
transaction across every database and file; an I/O failure may leave a partial fresh
install. Existing replacement rollback behavior is retained.

A human must confirm the plan before destination employee files are written.
AI sessions cannot import. `USER.md` enters prompts inside a `<user_profile>`
data block that explicitly says it contains data, not instructions. Its text is
scanned by the same input guard as persona text.

The dashboard export RPC is `agents.export_profile`; it requires Manager or
higher and access to the selected employee. The local operator command is:

```sh
duduclaw agent export customer-support --format duduclaw --out customer-support.agent.md
```

Use `--include-user` only when the user introduction should travel with the
profile. User is excluded by default in all formats. CLI export is refused in AI
sessions and participates in the Bash operator-command guard. This Bash guard is
a command-level check, not OS isolation from unrestricted shell access.

The exporter reads known regular files, rejects symlinks, bounds reads, selects
only permitted settings, and scans included sections and rendered output with the
shared credential-shape detector. A rejection names the section without echoing
the suspected secret. A successful export records an audit event. Credential
shape detection does not prove that arbitrary prose contains no sensitive data;
the human choosing what to export remains responsible for its content.

## Credential handling / 憑證處理 / 認証情報の扱い

Imported prompt-bearing documents, including SOUL, IDENTITY, USER, INSTRUCTIONS,
Contract, memory, knowledge and skill text, pass through the shared credential
masker before preview or resolved installation planning. Each profile section's
`input_guard` entry reports `masked_values` without exposing values. Preview,
report, confirmation digest and installed files use the same masked document
bytes; retained UTF-8 source copies are masked too. Re-previewing unchanged input
produces the same digest. Text scanning is bounded to 512 KiB per imported text
file; a scan failure or an oversized text file refuses the import with a
value-free error. Secret-shaped structured settings are refused by field path
(such as `duduclaw.model.preferred`), never rewritten. Credential-bearing metadata
is also refused. Dashboard and external/MCP `user` writes mask USER.md before
review, hashing, version history and baseline acceptance. Size limits apply
before and after masking. This does not scrub existing files, history or backups,
and credential detection does not identify every kind of private information.

匯入的提示文件（包含 SOUL、IDENTITY、USER、INSTRUCTIONS、Contract、記憶、知識與技能文字）會先經過共用憑證遮罩，再產生預覽或解析安裝計畫。各設定檔區段的 `input_guard` 以 `masked_values` 顯示遮罩數量，不顯示原值。預覽、報告、確認摘要與安裝檔案使用相同的遮罩文件位元組；保留的 UTF-8 來源副本也會遮罩。來源未變更時，重新預覽的確認摘要相同。每個匯入文字檔的掃描上限為 512 KiB；掃描失敗或文字超限時，以不含原值的錯誤拒絕匯入。結構化設定若含疑似憑證，會指出欄位路徑（例如 `duduclaw.model.preferred`）並拒絕，不會改寫設定值；中繼資料若含憑證也會拒絕。儀表板與外部／MCP 的 `user` 寫入，在審查、雜湊、版本保存與接受基準前先遮罩 USER.md；遮罩前後都檢查大小限制。此變更不會回溯清除既有檔案、歷史或備份，憑證偵測也無法辨識所有私人資訊。

取り込むプロンプト文書（SOUL、IDENTITY、USER、INSTRUCTIONS、Contract、記憶、知識、スキルのテキスト）は、プレビューや確定したインストール計画を作る前に共通の認証情報マスクを通します。各プロファイルセクションの `input_guard` は `masked_values` で件数だけを示し、元の値を表示しません。プレビュー、レポート、確認ダイジェスト、インストール先は同じマスク済み文書のバイト列を使用し、保存する UTF-8 ソースのコピーもマスクします。入力が同じなら再プレビューのダイジェストも同じです。取り込むテキストはファイルごとに 512 KiB まで検査し、検査失敗や上限超過では値を含まないエラーで取り込みを拒否します。構造化設定の認証情報は書き換えず、`duduclaw.model.preferred` などのフィールドパスを示して拒否します。認証情報を含むメタデータも拒否します。ダッシュボードと外部／MCP の `user` 書き込みでは、審査、ハッシュ計算、履歴保存、基準の承認より前に USER.md をマスクし、マスク前後にサイズ制限を確認します。既存ファイル、履歴、バックアップの遡及削除は行いません。また、認証情報検出がすべての個人情報を識別するわけではありません。

## Runtime prompt order

The shared profile loader supplies sections; each builder preserves its existing
contract precedence relative to contextual material:

| Builder | Contract position |
| --- | --- |
| `duduclaw-agent::runner` | After skills, legacy memory and wiki; before the dynamic suffix |
| `gateway::claude_runner` | After skills, legacy memory and wiki |
| `gateway::channel_reply` | Existing order preserved: profile contract before skills/wiki |
| `gateway::prompt_minimal` | Existing order preserved: end of profile, before sender/tool index |

Security checks cover plan changes (including tool restrictions, skill bodies and collision resolution), the two restored contract positions, and runtime versus import handling of legacy contracts.
