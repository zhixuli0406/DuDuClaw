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
| Handling old information | A new version of the same fact supersedes the old one automatically, with history preserved | Overwrites the whole page; auto-filed pages keep a one-line revision log at the bottom (see 2.4), manually written pages don't |
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

### 4.4 Deleting a memory

Hover over any entry in the memory list and a trash icon appears on the right; two clicks (the second confirms) deletes it. A deleted memory disappears immediately from search, browsing, and conversation injection.

Underneath, this is a soft delete: the record moves to an archive table, an administrator can still recover it from the database, and it's only purged for good after the retention window passes.

---

## 5. Which one should you use?

| What you want to do | How to do it |
|---|---|
| Have it remember you prefer short replies | Just say so — it lands in memory automatically |
| Set up a return policy it follows every time | Write it to the knowledge base and set `layer: core` |
| Organize today's research findings from a few papers | Write it to the knowledge base, under `sources/` |
| A policy every AI employee in the company must follow | Write it to the shared knowledge base |
| Correct something it remembered wrong | Just say the correct version — the old one gets superseded automatically |
| Remove one incorrect memory | Hover over it in the memory list and click the trash icon |
| Make it forget a whole document | Delete that page from the knowledge base |
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
