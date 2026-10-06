# Memory and knowledge base guide

Your AI employee remembers things on its own, and it can also organize knowledge into pages the way you ask it to. Both live on the same "Memory & Knowledge" (記憶與知識) page in the dashboard, but underneath they are two different systems with different behavior and different timing. This guide covers both in full.

The one-line distinction:

- **Memory**: what it records on its own. You don't have to say anything; real conversation content gets distilled and stored automatically.
- **Knowledge base**: a full document kept for reference. When you paste in a charter, an SOP, a spec, or similar long-term reference material, it gets organized into a page automatically; you can also say "file this in the knowledge base" to make it explicit. The knowledge base is the wiki, just a different name for the same thing.

---

## 1. What's the difference

| | Memory | Knowledge base (wiki) |
|---|---|---|
| How it fills up | Accumulates automatically, no instruction needed | Auto-filed when you paste a charter/SOP/spec-type document; or say "write this to the knowledge base" to be explicit |
| Stored as | Discrete facts | Markdown pages |
| Categorization | The system sorts by topic automatically | Directories (folders) plus the page path you give it |
| Handling old information | A new version of the same fact supersedes the old one automatically, with history preserved, unless the old one comes from a more trusted source (then it goes to review) | Overwrites the whole page; auto-filed pages keep a one-line revision log at the bottom (see 2.4), manually written pages don't |
| When it gets recalled | Three categories inject automatically, everything else needs an active lookup | L0/L1 auto-inject every turn, L2/L3 need an active search |
| Who can see it | Only this AI employee | Personal knowledge base is private to the agent; the shared knowledge base is readable company-wide |
| What belongs here | Scattered facts, preferences, and decisions that surface mid-conversation | Content worth looking up long-term: return policy, quoting process, product specs |

The selection rule is simple: **write it to the knowledge base if it needs to be searchable, editable, or shown to other people; let everything else land in memory on its own.**

---

## 2. Using the knowledge base

### 2.1 Creating a page: just say it in conversation

No need to open the dashboard to edit, and no syntax to memorize. On any connected channel (LINE / Telegram / Discord / Slack / web chat), tell the AI employee:

```
幫我把退貨規則記到知識庫：七天內未拆封可退，運費由買方負擔。
```

```
今天查到的三篇論文重點整理成一頁放知識庫，標題叫「RAG 檢索方法比較」。
```

It creates a Markdown page, updates the `_index.md` index automatically, and leaves one entry in `_log.md`.

**What if you don't say "knowledge base"?** It depends on the content. If what you pasted looks like a charter, an SOP, a spec, a policy, or similar long-term reference material, it judges the content and files it on its own (see 2.4); ordinary conversation, questions, and one-off requests only get stored as memory. The judgment leans conservative: it would rather skip a page and let you ask again than turn small talk into a document. Saying "file this in the knowledge base" explicitly always works — that path hasn't changed.

### 2.2 Categories: auto-selected, or you can specify

Every knowledge base has four default directories, and the AI employee picks one based on content when it writes:

| Directory | What goes here | Example |
|---|---|---|
| `entities/` | People, companies, products, customers | `entities/wang-ming.md` |
| `concepts/` | Domain concepts, processes, principles | `concepts/return-policy.md` |
| `sources/` | Summaries of raw material | `sources/2026-07-30-rag-papers.md` |
| `synthesis/` | Cross-topic analysis, comparisons, trends | `synthesis/vendor-comparison.md` |

To specify one yourself, just say so: "put it under `concepts/`, filename return-policy." Without an instruction it follows the table above, with filenames in kebab-case.

### 2.3 Layers: how often a page gets recalled

Every page's YAML frontmatter has a `layer` field — the single most important setting in the whole knowledge base:

| Layer | Value | Behavior |
|---|---|---|
| L0 Identity | `identity` | Auto-injected every conversation |
| L1 Core | `core` | Auto-injected every conversation |
| L2 Context | `context` | Not auto-injected; surfaces only through search |
| L3 Deep | `deep` | Not auto-injected; surfaces only through search (**default**) |

A page with no `layer` set defaults to L3. So a page that "got written but doesn't seem to get used" is most likely stuck at L3. To make it show up every time, say "set this page to core layer" or "set layer to core" in conversation.

Every page also carries a `trust` score (0.0–1.0). Search ranking weights by this score, so content that's been human-reviewed ranks higher.

### 2.4 Auto-filing: the path that needs no request

You paste a document, the AI replies "got it, saved," and until now nothing else would happen. Now it first judges whether the text is long-term reference material, and if so, organizes it into a page automatically.

**Criteria** (all must hold; the bar is set deliberately high): document-type nouns (charter, procedures, standard, SOP, spec, manual…), numbered clauses or section structure, sufficient length, a title line. Conversely, first-person preferences ("I like…"), time-bound requests ("remind me tomorrow…"), and back-and-forth questions all count against it and won't be treated as a document.

**Where auto-filed pages go**: inside this AI employee's own knowledge base, under `auto/`, split into five folders by type — charter `auto/charter/`, SOP `auto/sop/`, spec `auto/spec/`, policy `auto/policy/`, other `auto/reference/`. Your manually organized directories (`entities/`, `concepts/`, `sources/`, `synthesis/`) are never touched by auto-filing.

**How an auto-filed page differs from a confirmed one**:

- The page opens with a notice stating it was auto-organized and hasn't been confirmed by a person.
- The content preserves the original text verbatim, never rewritten. The cost of distorting a charter or a contract is too high.
- **It is never auto-injected into conversation.** Auto-filed pages sit at the L2 context layer, so the AI has to actively search for one to see it. Even a wrong judgment call can't pollute every answer.
- Search ranking weights it far lower than a human-written page.

**Pasting the same document a second time** updates the same page rather than creating a duplicate, and the revision log at the bottom gains one more line.

**Where to manage it**: dashboard → "Memory & Knowledge" (記憶與知識) → "Curation Station" (策展台) → "Auto-filed" (自動建檔). Each page supports:

| Action | Effect |
|---|---|
| "View" (檢視) | See the full page content and revision log |
| "Confirm as official knowledge" (確認為正式知識) | Promotes the page to content you've approved, so it starts auto-injecting every conversation |
| "Share to shared knowledge base" (分享到共享知識庫) | Copies the page to the shared area so other AI employees can read it too |
| "Remove" (移除) | Drops the page from the knowledge base; it moves to the archive and can be restored |

To turn auto-filing off entirely: drop a `.scope.toml` in that AI employee's knowledge base directory declaring `[namespaces.auto] mode = "operator_only"`. After that, only manual writes go through.

### 2.5 Personal vs. shared knowledge base

- **Personal knowledge base**: `~/.duduclaw/agents/<agent>/wiki/`, readable only by this AI employee.
- **Shared knowledge base**: `~/.duduclaw/shared/wiki/`, readable by every AI employee in the company. Company policy, shared SOPs, and product specs belong here.

To write to the shared area, say so explicitly: "put this in the shared knowledge base so everyone can see it."

The personal edition has only one knowledge base, so the dashboard doesn't show a "Personal/Shared" (個人／共享) tab switch.

---

## 3. When the knowledge base gets used

This is the part most likely to cause confusion. There are two retrieval paths.

**Auto-injection (L0 + L1)**: before every conversation turn, the system ranks identity-layer and core-layer pages by relevance to the current question and fits them into the system prompt within a 6 KB budget. Within the same conversation session, the selected pages stay fixed for 15 minutes so prompt caching still applies. You don't have to do anything.

**Active search (L2 + L3)**: everything else depends on the AI employee judging that a question warrants a knowledge-base check, then calling a search tool. Search is full-text, ranked by trust score and source type.

**So should you remind it to check?** Usually not. L0/L1 content is visible to it every time; for L2/L3 content, a matching keyword in your question is usually enough to trigger a search on its own.

Two situations are worth a nudge:

1. **It answers wrong or vaguely**, and you know the knowledge base has the right answer — say "check the knowledge base and answer again."
2. **Your wording is far from the page's wording** (say, the page says "return policy" and you ask "what if I don't want this anymore") — just naming the page is the fastest fix.

---

## 4. Using memory

### 4.1 What it remembers automatically

| Source | What it captures | Dashboard category |
|---|---|---|
| Conversation distillation | Facts, decisions, and preferences from real conversation | Filed by content under "Work / Client / Preference" (工作／客戶／偏好) |
| Key facts | Points that recur across multiple conversations | "Observations & Insights" (觀察洞察) tab |
| Learning signals | Gaps between expected and actual outcomes (how well it answered) | "Learning Signals" (學習訊號) |
| Usage footprint | Your app usage duration and active hours (opt-in) | "Usage Footprint" (使用足跡) |
| Mistake generalization | Rules generalized from a cluster of the same kind of mistake | "Rules & Decisions" (規則與決策) |

**What it won't remember**: greetings, short acknowledgments like "OK" or "got it," and small talk with no substantive content. A zero-cost classifier filters these out first.

**Duplicates no longer get stored twice** (as of v1.53): a new write at the semantic layer is compared against existing memories for similarity first; near-duplicates get rejected and logged as telemetry, so the same fact doesn't pile up as dozens of near-identical memories that dilute retrieval quality. Normal updates to an existing memory (a correction replacing what was said before, a reconfirmation) aren't affected, and memories you curate manually in the dashboard skip this gate entirely. Turn it off with `[memory] novelty_gate = false` in `config.toml` (on by default).

### 4.2 When memories get recalled

Three categories are auto-injected into the system prompt every conversation:

- **Key facts about you** (private conversations only; a shared group session blocks this, so personal information never leaks into a public setting)
- **Past mistakes** (unresolved mistakes of the same category)
- **Learned rules** (rules generalized from mistakes that have passed their observation window, capped at three. As of v1.53, only mistake records backed by actual tool-call evidence feed the generalization — an AI employee saying "that was my mistake" with no matching tool record behind it never produces a rule)

Everything else needs the AI employee to judge that a search is warranted and run it. Retrieval ranking weighs relevance, importance, and how long since a memory was last recalled; memories that get referenced often live longer.

### 4.3 Memory updates itself

When a new statement about the same topic comes in, the old one gets marked "superseded" and the new one takes its place. Expanding any memory shows the full supersession chain, and you can also ask "what was the answer as of a given point in time." So correcting course doesn't require deleting the old entry first; just say the new thing.

One exception (v1.67.1): a statement from a chat cannot replace a fact from a more trusted source, such as one you approved in the dashboard or one imported with `migrate from`. The new statement is held, and a review item appears in the dashboard inbox (收件匣) showing the current content and the new statement side by side. Approve it and the new statement replaces the old one; deny it and it is discarded. These items can only be decided in the dashboard, within 24 hours. Details: [Memory Intelligence](../features/20-memory-intelligence.md#supersession-trust-guard-v1671).

### 4.4 Deleting a memory

Hover over any entry in the memory list and a trash icon appears on the right; two clicks (the second confirms) deletes it. A deleted memory disappears immediately from search, browsing, and conversation injection.

Underneath, this is a soft delete: the record moves to an archive table, an administrator can still recover it from the database, and it's only purged for good after the retention window passes.

### 4.5 Forgetting a conversation, a scheduled run or an imported file

Use this when someone asks you to make an AI employee forget something they said, or when a scheduled run or an import put content into memory that should not be there. Deleting entries one at a time (4.4) only removes the entries you can see. This procedure removes every memory that came from one source, including the ones derived from it, and blocks the same source from being learned again. Every command below runs in your own terminal, not in an employee's session.

Before you start: the command only knows about memories written after the source-recording feature was installed. Older memories have no recorded source; the plan counts them (`沒有完整來源紀錄的記憶`) but never deletes them. The same count includes memories written by a dispatched run whose bus message carried only half of the upstream conversation's identity: they keep the run as their source, but forgetting the upstream conversation cannot reach them.

1. List the sources.

```bash
duduclaw memory forget-source list --agent sales-rep
```

The output has one line per conversation (or per series of scheduled runs, or per imported file): the session key, the kinds of source in brackets (`聊天訊息` channel messages, `員工自行存入` what the employee stored itself during a turn, `排程／派工執行` scheduled or dispatched runs, `外部 MCP 用戶端`, `匯入`, `足跡`), how many memories carry it, and when the latest one was written. A memory is counted once per line even when it has several sources in that conversation. To see the individual messages or runs of one session:

```bash
duduclaw memory forget-source list --agent sales-rep --session <session>
```

Here each line is one message or one run and ends with `→ --message <value>`, the value to pass to `--message`. What the employee stored itself during a turn is listed under the user message that started the turn (`含員工在這一輪自行存入的記憶`), because forgetting that message forgets the turn too; a turn whose message was not recorded gets its own `turn:` line.

`--agent` is the employee id, or `external/<client>` / `internal/<client>` for a memory namespace used by an MCP client.

2. Create a plan. Pick the form that matches what you need to forget.

| What to forget | Command |
|---|---|
| Specific messages | `duduclaw memory forget-source plan --agent sales-rep --session <session> --message 812,815` |
| A whole conversation, up to now | `duduclaw memory forget-source plan --agent sales-rep --session <session>` |
| One scheduled or dispatched run | `duduclaw memory forget-source plan --agent sales-rep --session <session> --message run:<key>` |
| An imported file | `duduclaw memory forget-source plan --agent sales-rep --session import:/path/to/file` |

`--message` takes the value after the arrow in `list --session`: a message number (`812` and `m:812` mean the same message) or a key such as `run:<key>`; `turn:`, `call:`, `item:` and `day:` keys are accepted the same way. Forgetting a user message also forgets the employee's reply in the same turn, because a reply often repeats what the user said, and whatever the employee stored itself during that turn: the turn counts as the same source, so a later write that names only the turn is blocked too. Forgetting a whole conversation covers every turn in it. `--show-snippets` prints the first 60 characters of each memory on screen (never stored). `--max-rows` and `--ttl-minutes` change the size limit (default 50,000 rows) and how long the plan stays valid (default 30 minutes, at most 1,440).

`plan` deletes nothing. It records the plan, files an approval request, and prints the plan. Read these parts (the command prints Chinese; the quoted phrases below are its exact wording):

- `將刪除 N 筆記憶、N 筆關鍵事實、N 份封存副本、N 個自動建檔頁面`: what will be deleted. Each target line shows the memory id, its kind and layer, and how it was reached: `直接` (the memory came from the source itself) or `衍生` (it was derived from something that came from it).
- `同一輪的員工回覆也一併忘記` and `個回合也一併設為不再學到`: the reply and the employee's own writes in the same turn, which are part of the same source. `show` and the `apply` preview print the same lines.
- `壓縮摘要`: a conversation has one compressed summary, which cannot be cut down to one message or one run, so every plan clears the whole conversation's summary (for a scheduled run, the whole `cron:<employee>` conversation). The line says so.
- `連帶影響`: other sources that also supported a memory slated for deletion. Those memories are deleted whole, so you lose what the other source said too. Memories that were only mentioned again by the forgotten source stay (`保留，僅移除佐證紀錄`).
- `需要人工檢視`: wiki pages the employee wrote itself whose recorded sources match. They are not deleted; open them and decide.
- `對話紀錄`: how many messages the employee will stop seeing. The original text stays in the conversation record.
- `其他命名空間也記錄了同一段對話`: ready-to-run `plan` commands for other employees that stored the same conversation. Each needs its own plan and approval. The command also reaches what an employee stored while doing work this turn handed to it, even after the first plan was applied.
- `套用時會設下 N 筆防止再學到的紀錄`: how many blocks the apply writes. Forgetting a long conversation writes one per turn in which the employee stored something, so the number can be in the thousands.
- `不在範圍內`: what this command does not reach. Read it every time (see the list at the end of this section).

3. Approve in the dashboard. An Admin opens the dashboard to-do list and approves the request named in the plan output. The card shows counts and source labels, never memory content, and states that the request came from a local command line. The approval is tied to this exact plan. Channel buttons cannot approve it.

To check where it stands without applying:

```bash
duduclaw memory forget-source apply --plan <plan-id>
```

Without `--confirm` this prints the plan again plus a line `核准狀態：` saying whether the approval is pending, granted or no longer valid.

4. Apply.

```bash
duduclaw memory forget-source apply --plan <plan-id> --confirm
```

On success it prints how many memories, key facts and archive copies were deleted, then the follow-up result. The follow-up steps happen outside the memory database: deleting auto-filed wiki pages, withdrawing review cards, hiding the forgotten messages from the employee, and clearing the conversation's compressed summary.

5. If the follow-up did not finish. A result of `DEGRADED` (exit status 3) means the memories are already deleted and the block is in place, but some follow-up steps failed. Run:

```bash
duduclaw memory forget-source resume --plan <plan-id>
```

A running gateway also retries unfinished steps every 10 minutes and at start-up. When nothing is left to run, `resume` says so (`沒有需要補跑的後續步驟`).

If the plan expired or the data changed. An apply refused with `計畫已過期`, or with a message saying the data changed since the plan, deleted nothing. A stale refusal lists every reason that applies at once: another forget in the same namespace, memories added, removed or changed, and other parts of the plan that changed (for example review cards or conversation records). Writes that do not change what the apply would do, such as the background archiving of old memories or another employee storing the same conversation, do not make the plan stale; the apply output shows the two informational counts (`沒有完整來源紀錄的記憶` and the other namespaces) as they were at plan time and at apply time. Pause the employee, run `plan` again and get a new approval. The approval expires together with the plan, so an expired plan always needs a new one.

Large plans. When a plan has more than 5,000 targets, `apply` locks the memory store for several seconds and the plan output says so. Pause the employee before applying.

What this does not do.

- It deletes memories derived from the source and blocks the system from learning from the same source again. It does not delete the conversation text (the employee just stops seeing it), messages already sent, backups, or content other employees received.
- Content can still reach the employee through places this command does not touch: tool-call records and error notes that are put into the prompt every turn, the task board and `/goal` text, working state, Agent Mail, goal state and judge feedback, hand-off copies, replies passed between employees, other kinds of review cards, the Claude CLI's own transcripts under the user's home directory, the reinforcement-learning trajectory files written after every reply (`rl_trajectories.jsonl` and the `rl_trajectories/` directory, which hold the whole conversation's text and carry no message number that would identify what to remove), shared wiki copies, and wiki pages the employee wrote itself. Those are never deleted: pages written after the feature was installed are listed for you to review, and older ones have no recorded source, so they are not listed at all. Memories an employee stored through the Gemini CLI runtime are not tied to a conversation and are not reached; whether Grok employees' memories are tied has not been verified.
- A turn is linked to its user message only when one of the employee's own writes in that turn recorded both as its sources (a write that only restated a memory it already had counts too). A turn in which the employee stored nothing has no recorded link, so forgetting the message does not reach a later write that names only that turn; forgetting the whole conversation still covers it.
- When a channel reply is answered by the local model first (`inference_mode = local`), what the employee stores through tools in that step is not tied to the conversation, so forgetting the conversation does not reach it.
- If the employee learns the same thing again in a new conversation, that is new information and is not blocked.
- Restoring a backup taken before the forget brings back the deleted memories and removes the block. Do not downgrade to a version that does not have this feature after using it.
- Setting `[memory] forget_source = false` in `config.toml` stops new plans and applies. It does not turn off the dashboard approval, which has no switch.

---

## 5. Which one should you use?

| What you want to do | How to do it |
|---|---|
| Have it remember you prefer short replies | Just say so — it lands in memory automatically |
| Set up a return policy it follows every time | Write it to the knowledge base and set `layer: core` |
| Organize today's research findings from a few papers | Write it to the knowledge base, under `sources/` |
| A policy every AI employee in the company must follow | Write it to the shared knowledge base |
| Correct something it remembered wrong | Just say the correct version — the old one gets superseded automatically; if the old one came from a more trusted source, approve the review item in the dashboard inbox |
| Remove one incorrect memory | Hover over it in the memory list and click the trash icon |
| Make it forget a whole document | Delete that page from the knowledge base |
| Someone asked it to forget a conversation, a scheduled run or an imported file | Forget by source, see 4.5 |
| Paste in a company charter so it can look it up later | Just paste it — it auto-files; once confirmed as official knowledge in the curation station, it injects every time |
| Remove one auto-filed page | Curation Station → Auto-filed → Remove |

---

## 6. FAQ

**Q: Are the knowledge base and the wiki different things?**
Same thing. The interface calls it "knowledge base"; the underlying file layout and MCP tool names still use "wiki."

**Q: Do I need to tell the AI employee "write this to the knowledge base"?**
Not for a charter, SOP, or spec-type document — it auto-files those (see 2.4). For everything else, yes. When the judgment call is uncertain it leans toward not filing, so if you want a page to stick, saying it explicitly is the fastest way.

**Q: Can the knowledge base be categorized? Or does it categorize itself?**
Both. The four default directories get chosen automatically based on content, and you can also specify a path yourself. The layer (L0–L3) defaults to L3; say so explicitly to change it.

**Q: When does it actually use the knowledge base? Should I remind it?**
L0/L1 auto-injects every conversation; L2/L3 relies on it searching on its own. You usually don't need to remind it. When it answers wrong or your wording is far from the page's, saying "check the knowledge base" is the most effective nudge.

**Q: What happens if memory and the knowledge base overlap?**
The knowledge base wins. When content gets injected, the system checks for overlap, and a fact already covered by a knowledge base page won't get pulled in again from memory.

**Q: Does memory grow without limit?**
No. Memories that go unrecalled for a long time and carry low importance get archived progressively; memories that are referenced often stick around.

---

## Related documents

- [`templates/wiki/_schema.md`](../../templates/wiki/_schema.md) — Full definition of the knowledge base page format and frontmatter fields
- [`docs/spec/soul-md-spec.md`](../spec/soul-md-spec.md) — SOUL.md persona file spec
- [`docs/guides/evals.md`](evals.md) — Behavioral regression testing (verifies memory and the knowledge base actually affect answers)
- [`docs/architecture/overview.md`](../architecture/overview.md) — Memory engine and retrieval architecture
