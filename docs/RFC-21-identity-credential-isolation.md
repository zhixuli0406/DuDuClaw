# RFC-21 — Identity Resolution & Per-Agent Credential Isolation

**Owner:** —
**Status:** Draft (architecture review)
**Tracking issue:** [#21](https://github.com/zhixuli0406/DuDuClaw/issues/21)
**Reporter:** Ruby-11235813
**Last updated:** 2026-05-04

---

## TL;DR

The reporter's deployment exposes three interlocking design gaps in DuDuClaw's
multi-agent isolation story. They are correctly characterised in #21 as a
violation of the *external-system-as-single-source-of-truth* principle.

| # | Sub-problem | Real fault | Proposed fix |
|---|---|---|---|
| 1 | Identity Resolution leaks to shared wiki | No `IdentityProvider` trait — agents hand-roll lookups via `shared_wiki_read`, so anyone not pre-listed in `shared/wiki/identity/discord-users.md` is a stranger | Introduce `IdentityProvider` trait + `NotionIdentityProvider` / `WikiCacheIdentityProvider` impls; expose `identity_resolve` MCP tool; wiki acts only as cache |
| 2 | Odoo MCP credential is global | One `[odoo]` block in `config.toml` decrypted into a process-wide `Arc<RwLock<OdooConnector>>`; every agent that passes MCP auth inherits the same `odooadmin` privilege | Per-agent `[odoo]` override in `agent.toml` + lazy per-call connector pool keyed by `(agent_id, profile)` + new `odoo:read` / `odoo:write` MCP scopes |
| 3 | Shared wiki has no SoT boundary | `shared_wiki_write` accepts any path under `~/.duduclaw/shared/wiki/`; nothing prevents an evolving agent from mirroring (and silently overriding) authoritative external data | Add `[wiki.shared.scope]` policy file: per-namespace `read_only` / `synced_from` / `agent_writable` flags enforced inside `handle_shared_wiki_write` |

The three fixes share one design principle: **system-layer authority, not
prompt-layer suggestion**. SOUL.md / CLAUDE.md instructions are best-effort;
the boundary must be enforced where the tool dispatcher runs, not where the
LLM reasons.

---

## Problem statement (verbatim summary of #21)

1. **Identity resolution walks shared wiki** — `shared/wiki/identity/discord-users.md`
   currently lists 2 people; everyone else (team members, customer contacts,
   engineers) is invisible. Project agents declare in SOUL.md "reject non-project
   members", but the agent has no `ProjectMember` roster and no mechanism to
   query the authoritative source (Notion People DB).
2. **Odoo MCP credential has no per-agent / per-project isolation** — `config.toml`
   declares one `username = "odooadmin"`; every agent decrypts the same admin
   key. SOUL.md "read-only" / "do not modify production" is prompt-layer
   self-restraint, not system enforcement. Cross-project data leakage and
   attribution loss in Odoo audit log are direct consequences.
3. **Shared wiki ↔ external SoT boundary undefined** — DuDuClaw's evolution
   loop tends to write things it learns into shared wiki; over time this drifts
   the wiki into an unmanaged copy of the external system, and the user has
   no knob to declare "identity / access-control namespaces are read-only,
   only the upstream sync may write here."

---

## Current state (verified)

Numbers below were verified by code search on 2026-05-04 against the
`v1.10.1` working tree.

### Identity surface

- MCP **caller** identity is fully realised:
  [crates/duduclaw-cli/src/mcp_auth.rs:39-44](crates/duduclaw-cli/src/mcp_auth.rs#L39-L44)
  — `Principal { client_id, scopes, is_external, created_at }`, validated at
  [mcp_auth.rs:171](crates/duduclaw-cli/src/mcp_auth.rs#L171) (`authenticate_with_key`)
  and bound at server start in [mcp.rs:5927-5931](crates/duduclaw-cli/src/mcp.rs#L5927-L5931).
- MCP **conversation-partner** identity (Discord user → real person) is
  **absent**. There is no provider trait, no resolver entry point, no
  `identity_resolve` MCP tool. The only thing an agent can do is call
  `shared_wiki_read` on a known path — exactly what #21 reports.
- SOUL.md "ProjectMember" wording is written by `create_agent` at
  [mcp.rs:2709-2711](crates/duduclaw-cli/src/mcp.rs#L2709-L2711) but never parsed
  back; it is plain text injected into the system prompt.

### Odoo credential surface

- `OdooConfig` is one struct, one `[odoo]` block:
  [crates/duduclaw-odoo/src/config.rs:7-35](crates/duduclaw-odoo/src/config.rs#L7-L35).
  `from_toml` ([config.rs:73-78](crates/duduclaw-odoo/src/config.rs#L73-L78))
  knows nothing about agents.
- The connector is global — `Arc<RwLock<Option<OdooConnector>>>` at
  [mcp.rs:6064](crates/duduclaw-cli/src/mcp.rs#L6064). `handle_odoo_connect`
  ([mcp.rs:5103](crates/duduclaw-cli/src/mcp.rs#L5103)) decrypts once, stores
  once, and every subsequent `odoo_*` tool call reads that single connector.
- The 15 Odoo MCP tools at [mcp.rs:558-668](crates/duduclaw-cli/src/mcp.rs#L558-L668)
  perform **no scope check**. The existing `Scope` enum
  ([mcp_auth.rs:14-22](crates/duduclaw-cli/src/mcp_auth.rs#L14-L22)) covers
  `MemoryRead/Write`, `WikiRead/Write`, `MessagingSend`, `Admin` — there is
  no `OdooRead` / `OdooWrite`.
- Per-agent crypto isolation does exist in spirit
  ([crates/duduclaw-security/src/crypto.rs](crates/duduclaw-security/src/crypto.rs))
  but the keyfile is machine-level (`~/.duduclaw/.keyfile`,
  [mcp.rs:5833-5843](crates/duduclaw-cli/src/mcp.rs#L5833-L5843)). The
  reporter's claim — "only encryption is isolated, not authority" — is
  factually correct.

### Shared wiki write surface

- `handle_shared_wiki_write` ([mcp.rs:7701-7760](crates/duduclaw-cli/src/mcp.rs#L7701-L7760))
  enforces: path safety, size cap, sensitive-pattern scan, frontmatter
  presence, fallback-content rejection, author attribution. It does **not**
  enforce: namespace ownership, read-only namespaces, or external-sync
  origin.
- Wiki "category" exists only as a path prefix (`identity/`, `access/`,
  `SOP/`, ...). There is no enum, no policy table, no per-namespace ACL.
- `wiki_visible_to` ([mcp.rs:6666-6688](crates/duduclaw-cli/src/mcp.rs#L6666-L6688))
  is per-agent visibility for the *agent-private* wiki; it is not consulted
  for `shared_wiki_*`.

---

## §1 — Identity Resolution provider abstraction

### Goal

Allow an operator to declare "the authoritative identity source for this
deployment is Notion People DB" (or LDAP / Postgres / a custom HTTP service),
and have **every** agent resolve a Discord user / LINE user / email
through that source — with the shared wiki demoted to a transparent cache.

### Proposed design

1. **New crate** `duduclaw-identity` (small, no `tokio` runtime
   dependency in the trait):

    ```text
    crates/duduclaw-identity/
    ├── src/
    │   ├── lib.rs              // re-exports
    │   ├── provider.rs         // IdentityProvider trait
    │   ├── principal.rs        // ResolvedPerson struct
    │   ├── providers/
    │   │   ├── notion.rs       // NotionIdentityProvider
    │   │   ├── wiki_cache.rs   // WikiCacheIdentityProvider (existing behaviour)
    │   │   ├── chained.rs      // ChainedProvider (cache → upstream)
    │   │   └── http.rs         // GenericHttpIdentityProvider
    │   └── error.rs
    └── Cargo.toml
    ```

2. **Trait surface** (illustrative; final shape during implementation):

    ```rust
    #[async_trait]
    pub trait IdentityProvider: Send + Sync {
        async fn resolve_by_channel(
            &self,
            channel: ChannelKind,        // Discord / LINE / Telegram / Email / ...
            external_id: &str,           // discord_user_id, line_user_id, ...
        ) -> Result<Option<ResolvedPerson>, IdentityError>;

        async fn lookup_project_members(
            &self,
            project_id: &str,
        ) -> Result<Vec<ResolvedPerson>, IdentityError>;

        fn name(&self) -> &str;          // for audit log
    }

    pub struct ResolvedPerson {
        pub person_id: String,           // stable canonical id (e.g. Notion page id)
        pub display_name: String,
        pub roles: Vec<String>,          // ["engineer", "customer-pm"]
        pub project_ids: Vec<String>,    // memberships
        pub emails: Vec<String>,
        pub channel_handles: HashMap<ChannelKind, String>,
        pub source: String,              // "notion" / "wiki-cache" / ...
        pub fetched_at: DateTime<Utc>,
    }
    ```

3. **Configuration** in `config.toml`:

    ```toml
    [identity]
    provider = "chained"               # chained → cache → upstream
    cache    = "wiki"                  # uses ~/.duduclaw/shared/wiki/identity/
    upstream = "notion"

    [identity.notion]
    database_id     = "abc123…"
    api_key_enc     = "…"
    refresh_seconds = 300
    field_map       = { name = "Name", roles = "Role", projects = "Project" }
    ```

    Falls back gracefully — if `[identity]` is absent, behaviour is the
    *current* "wiki only" mode, so existing deployments don't break.

4. **MCP tool surface** (additions):

    | Tool | Scope | Behaviour |
    |---|---|---|
    | `identity_resolve` | `identity:read` | `{channel, external_id}` → `ResolvedPerson` JSON, or `null` |
    | `identity_list_project_members` | `identity:read` | `{project_id}` → `[ResolvedPerson]` |
    | `identity_invalidate_cache` | `identity:write` | Manually invalidate one or all entries |

    `Scope` enum extended:
    [crates/duduclaw-cli/src/mcp_auth.rs:14-22](crates/duduclaw-cli/src/mcp_auth.rs#L14-L22)
    gains `IdentityRead` / `IdentityWrite`.

5. **Auto-injection into channel reply path** — when a channel message
   arrives at [crates/duduclaw-gateway/src/channel_reply.rs] the gateway
   resolves the sender once, attaches the result to the dispatch context,
   and the system prompt template gains a `<sender>` block:

    ```xml
    <sender>
      <person_id>person_2f9…</person_id>
      <name>Ruby Lin</name>
      <roles>customer-pm</roles>
      <project_ids>proj-alpha, proj-beta</project_ids>
      <source>notion</source>
    </sender>
    ```

   This means SOUL.md rules like "reject non-project members" become
   evaluable from data the agent already has, instead of requiring a wiki
   lookup mid-reasoning.

6. **Wiki cache demotion** — `WikiCacheIdentityProvider` is the *only*
   component allowed to write `shared/wiki/identity/`. When chained, it
   serves cached entries, and writes new entries on every successful upstream
   resolve (TTL configurable). All other write paths into that namespace are
   denied (enforced by §3 below).

### Migration plan

| Step | Change | Backward compat |
|---|---|---|
| 1 | Land `duduclaw-identity` crate with `WikiCacheIdentityProvider` only | No-op semantically — same wiki reads as today |
| 2 | Wire `identity_resolve` MCP tool, gated behind `identity:read` scope. Emit one-line audit on every resolve. | New tool; no existing behaviour broken |
| 3 | Implement `NotionIdentityProvider`, document `[identity.notion]` config | Opt-in; `[identity]` absent ⇒ current behaviour |
| 4 | Inject `<sender>` block into system prompt when provider configured | Adds a few hundred tokens; off by default; emits to cache log on first run |
| 5 | Update SOUL.md template generator to reference `<sender>` block instead of "check shared/wiki/identity/" | Existing SOUL.md untouched; only new agents get the new template |

### Acceptance criteria

- [ ] `identity_resolve` returns the same person for the same `(channel, external_id)` whether resolved against Notion live or the wiki cache.
- [ ] With provider unset, `shared_wiki_read("identity/discord-users.md")` continues to work (no regression).
- [ ] When Notion is reachable, an unknown Discord user is resolved within 300 ms (cache miss path) and within 5 ms on subsequent calls (cache hit).
- [ ] When Notion is unreachable, the chained provider degrades to wiki-cache-only and emits a warning to the unified audit log; the channel reply still proceeds.
- [ ] Identity resolution emits one row per call to `audit.unified_log` (`source: "identity"`, fields: provider, channel, external_id_hash, hit/miss, person_id_hash).

### Risks

- **Notion rate limit** — must back off on 429; cache TTL must be configurable.
- **PII in audit log** — hash external_ids in audit log, never log full names verbatim.
- **Provider drift** — if Notion field schema changes, resolver fails open or closed? Default: fail *closed* for `lookup_project_members`, fail *open* (return `None`) for `resolve_by_channel`. This matches the current "unknown user" semantic.

---

## §2 — Per-agent / per-project Odoo credential isolation

### Goal

Each agent should connect to Odoo with its own service account at its own
permission level. Cross-project data isolation must be enforced by the Odoo
server's ACL, not by the LLM's restraint. Audit log entries inside Odoo must
attribute every operation to the correct agent.

### Proposed design

1. **`agent.toml [odoo]` override block** — fully optional, falls back to
   the global `config.toml [odoo]` when absent:

    ```toml
    # agents/proj-alpha-pm/agent.toml
    [odoo]
    profile         = "alpha"            # arbitrary label; appears in audit log
    username        = "agent_alpha_pm"
    api_key_enc     = "…"                # encrypted with the same per-agent crypto path
    allowed_models  = ["crm.lead", "sale.order", "project.task"]
    allowed_actions = ["read", "search", "write:crm.lead"]   # no execute by default
    company_ids     = [1, 2]             # multi-company narrowing
    ```

   `allowed_models` and `allowed_actions` are **defence-in-depth**: even if
   the Odoo service account is mis-provisioned upstream, DuDuClaw will refuse
   the call before it reaches Odoo.

2. **`OdooConfig` becomes layered**:

    ```rust
    pub struct OdooConfigResolver {
        global: OdooConfig,                                  // existing
        per_agent: HashMap<String, AgentOdooConfig>,         // loaded at startup
    }

    impl OdooConfigResolver {
        pub fn for_agent(&self, agent_id: &str) -> ResolvedOdooConfig { … }
    }
    ```

   Edit [crates/duduclaw-odoo/src/config.rs](crates/duduclaw-odoo/src/config.rs)
   and a new `crates/duduclaw-odoo/src/agent_config.rs`.

3. **Connector pool, not singleton** — replace the global
   `Arc<RwLock<Option<OdooConnector>>>` at
   [mcp.rs:6064](crates/duduclaw-cli/src/mcp.rs#L6064) with:

    ```rust
    pub struct OdooConnectorPool {
        // key: (agent_id, profile)
        pool: DashMap<(String, String), Arc<OdooConnector>>,
        // tokio::sync::Mutex per slot for first-use connect
    }
    ```

   `handle_odoo_*` ([mcp.rs:5093 onwards](crates/duduclaw-cli/src/mcp.rs#L5093))
   take the calling agent's `Principal`, derive `(agent_id, profile)`, and
   look up / lazily build the right connector.

4. **New scopes** in [crates/duduclaw-cli/src/mcp_auth.rs:14-22](crates/duduclaw-cli/src/mcp_auth.rs#L14-L22):

    ```rust
    Scope::OdooRead          // odoo:read
    Scope::OdooWrite         // odoo:write
    Scope::OdooExecute       // odoo:execute   (workflow buttons / payments)
    ```

   Tool registration table at [mcp.rs:558-668](crates/duduclaw-cli/src/mcp.rs#L558-L668)
   gains a `required_scope` field; dispatcher rejects with
   `POLICY_DENIED` (re-using the existing governance error code) when the
   caller's `Principal.scopes` does not include the tool's scope.

5. **Audit attribution** — every `odoo_*` MCP call appends to
   `~/.duduclaw/audit/odoo_calls.jsonl` (then merged via
   `audit.unified_log`):

    ```json
    {"ts":"2026-05-04T11:14:23Z","agent_id":"proj-alpha-pm",
     "profile":"alpha","odoo_user":"agent_alpha_pm","model":"crm.lead",
     "action":"search_read","domain_hash":"…","record_count":12}
    ```

   This restores attribution that #21 correctly identifies as missing.

### Migration plan

| Step | Change | Backward compat |
|---|---|---|
| 1 | Introduce `Scope::Odoo*` in `mcp_auth`. Default-grant `OdooRead+OdooWrite+OdooExecute` to all existing keys at first boot, write back to registry. | All existing agents keep working unchanged. |
| 2 | Add `required_scope` to tool registry and enforce in dispatcher. | Step 1 ensures no agent loses access. |
| 3 | Land `agent.toml [odoo]` parsing + `OdooConfigResolver`. When agent has no override, falls back to global. | Identical behaviour for unconfigured agents. |
| 4 | Land `OdooConnectorPool`, deprecate global `OdooState`. | Pool with single key `(agent_id, "default")` is functionally equivalent in single-agent deployment. |
| 5 | Operators rotate global admin key into per-agent service-account keys (documentation + `duduclaw odoo migrate-keys` helper CLI). | Manual operator step; required for multi-tenant deployments only. |
| 6 | Once an operator declares "all agents migrated", remove the default-grant in step 1 and require explicit scope in `mcp_keys`. | Behind a config flag — operator opt-in. |

### Acceptance criteria

- [ ] An agent without `[odoo]` override and with no explicit Odoo scopes in its API key is **denied** all `odoo_*` calls (after step 6).
- [ ] Two agents with different `agent.toml [odoo]` profiles connect to Odoo as different `username` values; Odoo's `res.users` audit shows two distinct actors.
- [ ] `odoo_search` against a model not in `allowed_models` returns `POLICY_DENIED` before any HTTP call leaves the process — verified via wiremock test in `duduclaw-odoo`.
- [ ] Attempting to call `odoo_payment_status` (write-class) from a key with only `odoo:read` is rejected.
- [ ] `audit.unified_log` filtered by `source=odoo` shows one row per MCP call with `agent_id`, `profile`, `model`, `action`.

### Risks

- **Connection storm** — N agents = N HTTP-keep-alive pools to Odoo. Mitigation: pool reuses connector across calls per `(agent_id, profile)`; idle eviction after 10 min.
- **Migration drag** — operators currently relying on a single admin account need a clear playbook. Provide `duduclaw odoo profile add` CLI to script the rotation.
- **Encryption keyfile is still machine-level** — out of scope for this RFC; flagged as future work (HSM / per-agent keyrings).

---

## §3 — Shared wiki Source-of-Truth boundary

### Goal

Let an operator declare "namespace `identity/` and `access/` are read-only,
populated only by the upstream sync; agents may read but not write."
Without this, even after §1 lands, an evolving agent can still "helpfully"
duplicate an external person record into the wiki and create a divergence.

### Proposed design

1. **New file** `~/.duduclaw/shared/wiki/.scope.toml` (single source of
   namespace policy; loaded by gateway at startup, hot-reloadable):

    ```toml
    [namespaces."identity"]
    mode         = "read_only"          # agents may read; writes denied
    synced_from  = "identity-provider"  # only the IdentityProvider may write here
    last_sync_ok = "2026-05-04T11:00:00Z"

    [namespaces."access"]
    mode         = "read_only"
    synced_from  = "policy-registry"    # only governance policy bundle may write

    [namespaces."SOP"]
    mode         = "agent_writable"     # current default — full self-evolution allowed

    [namespaces."policies"]
    mode         = "operator_only"      # not even the IdentityProvider may write;
                                         # only `duduclaw wiki sync` CLI
    ```

2. **Enforcement in `handle_shared_wiki_write`** — extend
   [crates/duduclaw-cli/src/mcp.rs:7701-7760](crates/duduclaw-cli/src/mcp.rs#L7701-L7760)
   with one early-return check after path validation:

    ```rust
    let namespace = top_level_dir(&page_path); // e.g. "identity"
    match scope_policy.mode_for(namespace) {
        Mode::AgentWritable => { /* fall through to existing checks */ }
        Mode::ReadOnly { synced_from } if caller.is(synced_from) => { /* allow */ }
        Mode::ReadOnly { .. } => return error(POLICY_DENIED, "namespace is read-only"),
        Mode::OperatorOnly if caller.is_operator_cli() => { /* allow */ }
        Mode::OperatorOnly => return error(POLICY_DENIED, "operator-only namespace"),
    }
    ```

   `caller.is(synced_from)` is implemented by signing the writer principal
   with a one-shot internal capability when `IdentityProvider` (or the
   policy bundle loader) calls `WikiStore::write_page_with_author` —
   external MCP callers cannot forge it.

3. **`wiki_namespace_status` MCP tool** (read-only, scope `wiki:read`):
   returns the current `.scope.toml` so agents can see, before writing,
   whether a target namespace is writable. This dramatically reduces the
   "agent tries to write, gets denied, retries forever" failure mode.

4. **Defaults are conservative but non-breaking**:
   - First boot of an existing deployment: `.scope.toml` is **not auto-created**
     — absence ⇒ current "everything writable" behaviour. No regression.
   - `duduclaw wiki scope init` CLI creates the file with sensible defaults
     (`identity/` + `access/` read-only, everything else writable).

### Migration plan

| Step | Change | Backward compat |
|---|---|---|
| 1 | Add `.scope.toml` parser + `WikiNamespacePolicy` struct. If file absent ⇒ `Mode::AgentWritable` for everything. | No semantic change. |
| 2 | Add enforcement check in `handle_shared_wiki_write`. | No-op until operator creates `.scope.toml`. |
| 3 | Add `wiki_namespace_status` MCP tool + `duduclaw wiki scope init` CLI. | New surface only. |
| 4 | Document recommended defaults in `docs/features/17-wiki-knowledge-layer.md`. | Documentation. |
| 5 | When IdentityProvider lands (§1), make `WikiCacheIdentityProvider` the registered writer for `identity/` namespace. | Only meaningful after §1 ships. |

### Acceptance criteria

- [ ] With no `.scope.toml`: every `shared_wiki_write` that succeeded in v1.10.1 still succeeds.
- [ ] With `identity/` declared `read_only, synced_from = "identity-provider"`: a direct `shared_wiki_write` for `identity/foo.md` from an arbitrary agent is denied with `POLICY_DENIED`. The `WikiCacheIdentityProvider` write path still succeeds.
- [ ] `wiki_namespace_status` returns the current policy; round-trips through `serde` deterministically.
- [ ] `.scope.toml` hot-reloads when modified; no gateway restart needed (mirrors `policies/global.yaml` reload pattern in `duduclaw-governance`).
- [ ] Audit log row on every denied write: `audit.unified_log` `source=wiki`, `event_type=write_denied`, `reason=read_only_namespace`.

### Risks

- **Lock-out by misconfiguration** — operator declares `SOP/` read-only and breaks evolution. Mitigation: `duduclaw wiki scope check` CLI dry-runs current policy against last 7 days of `audit.unified_log` and warns of would-be-denied writes.
- **Hot-reload race** — file is being rewritten while gateway parses it. Mitigation: write-then-rename atomic update (existing pattern from `duduclaw-governance` policy reload).

---

## Cross-cutting: rollout strategy

The three sections are independently shippable, but their value compounds.
Recommended order:

1. **§3 Wiki SoT boundary first** (smallest blast radius, no new external
   integration, immediate operator-side win).
2. **§1 Identity Resolution** (largest user-visible value; depends on §3
   only for the wiki-cache write protection — the `IdentityProvider` itself
   is independent).
3. **§2 Odoo credential isolation** (largest internal refactor; benefits
   most from the scope-enforcement plumbing already exercised by §1).

A single Sprint covering all three is not realistic; budget across two
Sprints with §3 + §1-step-1-2 in the first, §1-step-3-5 + §2 in the
second.

## Out of scope

The following appear adjacent but are deliberately **not** addressed by
this RFC:

- Per-agent encryption keyfile (currently machine-level
  `~/.duduclaw/.keyfile`). Tracked separately as future security work.
- Notion as a full bidirectional integration. This RFC only treats Notion
  as a *read* source for identity; writing back to Notion (e.g. recording
  agent decisions as Notion rows) is a separate feature.
- Channel-level identity (verifying the Discord *token* belongs to the
  declared user). DuDuClaw trusts Discord's `user_id`; impersonation at the
  Discord layer is the user's responsibility.
- Replacing CLAUDE.md's existing "Per-Agent 密鑰隔離" wording — the
  documentation will be re-stated to match the post-§2 reality once §2
  lands.

## References

- Issue [#21](https://github.com/zhixuli0406/DuDuClaw/issues/21) — original report by Ruby-11235813
- [`crates/duduclaw-cli/src/mcp_auth.rs`](crates/duduclaw-cli/src/mcp_auth.rs) — existing `Principal` / `Scope` model (extended by §1, §2)
- [`crates/duduclaw-cli/src/mcp.rs`](crates/duduclaw-cli/src/mcp.rs) — MCP tool dispatcher (touched by all three sections)
- [`crates/duduclaw-odoo/src/config.rs`](crates/duduclaw-odoo/src/config.rs) — current single-config model (refactored by §2)
- [`docs/features/17-wiki-knowledge-layer.md`](docs/features/17-wiki-knowledge-layer.md) — current shared-wiki documentation (updated by §3)
- [`docs/TODO-agent-honesty.md`](docs/TODO-agent-honesty.md) — sister effort: prompt-layer claims vs. system-layer enforcement (same overarching philosophy as this RFC)
