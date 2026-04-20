# Git Worktree L0 Isolation

> The lightweight sandbox — one filesystem per task, atomic merges, no containers required.

---

## The Metaphor: A Restaurant's Mise en Place

Before service, a chef sets up individual stations — each with its own cutting board, knives, and prepped ingredients. Two cooks working on different dishes don't reach for the same shallots or scrape their onions onto each other's boards.

When a dish is finished, the plate goes to the pass and the station is cleared for the next order. If a cook ruins a dish mid-cook, their station is cleaned out — but the rest of the kitchen keeps running.

A container sandbox is like building an entire *separate kitchen* for each cook — expensive, overkill for simple prep. Git worktree isolation is just giving each cook their own station in the same kitchen. Cheap, fast, still isolated where it matters.

---

## Why Worktrees, Not Just Branches

Multiple concurrent agents editing the *same* checkout will clobber each other's files. The obvious fix is giving each agent its own branch. But a single branch is still just one working directory — switching branches tramples uncommitted work.

`git worktree` solves this by giving each branch its **own filesystem directory**:

```
Main repo:        /Users/you/project/
                  ↓
Branch main:      /Users/you/project/                  ← always here
Worktree 1:       /Users/you/project-wt/agent-a-swift-fox/
Worktree 2:       /Users/you/project-wt/agent-b-calm-pine/
Worktree 3:       /Users/you/project-wt/agent-c-eager-leaf/
```

All three directories share the same `.git/` object store (so it's storage-efficient), but each has its own working files and HEAD. Three agents can edit simultaneously without collision.

DuDuClaw calls this **L0 isolation** — cheaper than L1 container sandbox, stronger than just "hope the agents don't collide."

---

## The WorktreeManager

`WorktreeManager` provides the full lifecycle:

```
Agent task starts
     |
     v
create(agent_id, task_id)
     |
     v
├─ Generate branch name: wt/{agent_id}/{adjective}-{noun}
├─ Check resource limits (max 5/agent, 20 total)
├─ git worktree add <path> <branch>
├─ copy_env_files(.env, config.local, ...)
└─ Return WorktreeHandle
     |
     v
[Agent executes — reads/writes ONLY in its worktree]
     |
     v
inspect() — did the execution succeed?
     |
     v
merge() or cleanup() based on AgentExitCode
```

### Friendly Branch Names

Branch names come from a 50×50 word-pair generator — adjective + noun:

```
wt/duduclaw-pm/swift-fox
wt/xianwen-coder/calm-pine
wt/agnes/bright-hawk
wt/sam/crisp-river
```

Words are curated to be short, memorable, and non-offensive. At 2,500 combinations × agent scope, collisions are rare and cleanup is trivial.

### Resource Limits

```
MAX_WORKTREES_PER_AGENT: 5
MAX_TOTAL_WORKTREES:     20
```

If an agent hits its cap, new worktree creation fails fast rather than letting worktrees accumulate silently and exhaust disk.

---

## The Snap Workflow

Worktrees aren't just for *creating* space — they also define a **controlled merge protocol**. Inspired by the agent-worktree project, DuDuClaw uses a four-stage workflow:

```
┌─────────────────────────────────────────────────────┐
│  1. CREATE                                          │
│     └─ worktree + branch + env file copy            │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  2. EXECUTE                                         │
│     └─ Agent does work inside the worktree          │
│        (container sandbox nested here if needed)    │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  3. INSPECT                                         │
│     └─ Pure decision function (testable)            │
│        AgentExitCode + diff + test results          │
│        → Decide: merge / cleanup / keep-alive       │
└─────────────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────────────┐
│  4. MERGE or CLEANUP                                │
│     └─ Atomic merge with dry-run pre-check          │
│        or remove worktree + branch                  │
└─────────────────────────────────────────────────────┘
```

The pure-function decision logic (Stage 3) is separated from I/O so it can be unit tested with fixtures — no git operations required in tests.

### Structured Exit Codes

`AgentExitCode` is a typed enum so callers don't inspect numeric codes:

```
AgentExitCode {
  Success,       → auto-merge (if configured)
  Error,         → cleanup, don't merge
  Retry,         → keep worktree, re-run
  KeepAlive,     → leave for manual inspection
}
```

---

## Atomic Merge with Pre-Check

The merge step is the most dangerous — two concurrent agents both trying to merge into `main` at the same time would corrupt git state. DuDuClaw solves this with a **global merge mutex** and a **dry-run pre-check**:

```
merge(worktree_handle):
     |
     v
acquire global_merge_lock()        ← serializes all merges
     |
     v
git merge --no-commit --no-ff <branch>   ← dry-run
     |
     v
if conflicts or errors:
    git merge --abort
    return MergeResult::Conflicts(info)
     |
     v
git merge --abort                  ← still dry-run, rollback
git merge <branch>                 ← now commit for real
     |
     v
release global_merge_lock()
```

The `Mutex<()>` lives in a `OnceLock<Mutex<()>>` so **all** `WorktreeManager` instances across all threads share the same lock. Without it, `dispatch_in_worktree` calls from different async tasks would each create their own manager and race.

---

## copy_env_files: Safely Copying Secrets

When a worktree is created, certain files from the main repo need to travel with it — typically `.env`, `.env.local`, or config files. But naive copying opens three attack surfaces:

1. **Path traversal** — `.env/../../etc/passwd`
2. **Symlinks** — `.env → /etc/shadow`
3. **Oversized files** — `.env` weighing 500MB

`copy_env_files` jails all three:

```
for each file in allowlist:
     |
     v
canonical_path = fs::canonicalize(main_repo_path + file)
     |
     v
if !canonical_path.starts_with(main_repo_path):
    reject (path traversal)
     |
     v
if fs::symlink_metadata(canonical_path).is_symlink():
    reject (symlink attack)
     |
     v
if fs::metadata(canonical_path).len() > 1 MB:
    reject (oversize)
     |
     v
copy to worktree_path/file
```

The cost of getting this wrong once is potentially exposing host credentials to an agent's sandbox — so the jail is strict.

---

## Sanitization of Agent IDs

Agent IDs come from user input (`agent create my_agent!!!`). Branch names have strict charset rules. Before forming a branch name:

```
sanitize("My_Agent!!!")
     |
     v
lowercase     → "my_agent!!!"
     |
     v
replace [^a-z0-9-] with '-'
              → "my-agent---"
     |
     v
collapse multiple -
              → "my-agent-"
     |
     v
strip leading/trailing -
              → "my-agent"
```

This prevents malformed branch names from breaking git.

---

## Where Worktrees Fit in the Isolation Hierarchy

DuDuClaw has multiple isolation layers — use the cheapest one that's sufficient:

| Layer | Mechanism | Cost | Isolates |
|-------|-----------|------|----------|
| **L0 Worktree** | `git worktree` | Very low | Working files between concurrent agents |
| **L1 Container** | Docker / Apple Container / WSL2 | Medium | Filesystem + network + process tree |
| **L2 Capability Deny** | `agent.toml [capabilities]` | None (policy) | Tool access (bash / browser / computer use) |
| **L3 Hooks** | Claude Code PreToolUse hooks | Low | Runtime command-level blocking |

A pure "refactor this function" task only needs L0 — the agent edits files; no network calls. A "install dependencies and run tests" task needs L0 + L1 — the filesystem isolation *plus* a container network boundary.

---

## Configuration

Per-agent in `agent.toml`:

```toml
[container]
worktree_enabled       = true
worktree_auto_merge    = true   # auto-merge on Success exit code
worktree_cleanup_on_exit = true # remove on Error/Success
worktree_copy_files    = [".env", ".env.local", "config.local.json"]
```

If `worktree_enabled = false`, tasks run in the main checkout directory (historical behavior, acceptable for single-agent deployments).

---

## Observability

`WorktreeManager` emits tracing spans at every lifecycle stage:

```
INFO  worktree::create{agent_id=dudu branch=wt/dudu/swift-fox}
DEBUG worktree::copy_env_files{files=[.env, .env.local]}
INFO  worktree::execute_start{task_id=t_abc123}
INFO  worktree::execute_end{exit_code=Success duration=12.3s}
INFO  worktree::merge{dry_run=true conflicts=0}
INFO  worktree::merge{committed=true hash=abc1234}
INFO  worktree::cleanup{removed_branch=true removed_dir=true}
```

These flow through BroadcastLayer → WebSocket → Dashboard Logs page, so you can watch worktrees spin up and merge in real time.

---

## Why This Matters

### Concurrency Without Containers

Running 5 agents simultaneously used to mean either:
- One main checkout, with agents colliding on files (fast, broken), or
- Five container sandboxes (isolated, expensive to spin up).

Worktrees give you the isolation of the second with nearly the cost of the first.

### Atomic, Auditable Merges

The dry-run pre-check means a broken merge never half-applies. Either the merge succeeds cleanly or it's fully aborted. No mixed state. No "some files merged and some didn't."

### Composable with Other Layers

An agent can run in a worktree *inside* a container sandbox — worktree gives the filesystem isolation, container gives the network/process isolation. They stack.

### Human-Friendly Branch Names

`wt/dudu-pm/swift-fox` is memorable and greppable in logs. Beats opaque hashes like `wt-3d8b2a...`.

---

## The Takeaway

Five agents working simultaneously on the same codebase need *some* kind of isolation. Containers are overkill for most tasks. Raw branches aren't enough. Git worktrees hit the sweet spot — cheap, concurrent, with an atomic merge protocol that can't corrupt the main repo. It's the default isolation layer for every DuDuClaw agent task.
