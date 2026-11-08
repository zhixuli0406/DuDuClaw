# F6: restore a tombstoned protected employee (v1.81.0, design)

Status: design r2, 2026-11-08, not implemented. Code read at `feat/iso181-w12-20261108` @ 5b067e31d
(paths under `crates/`). F5 decision 4: restore a tombstoned employee to the same id, only for the
original, still-employed owner, while no new employee holds the name. r2 folds in the security review
of 2026-11-08 (baton run `20261108-191211-claude-18045-173`, GO with changes; citations checked on hx1,
not w12) and the owner decisions (§2). Review ids stay next to each rule: H1–H5 are in the design before
F6a–F6d start; M/L items land with the implementation, each with its §6 test. [SEC] = Daybreak review
before merge. Inferences are marked "(inferred, verify)".

## 1. What exists today

| Fact | Where |
|---|---|
| Tombstone is Personal-only: Shared employees get `Denied` and the RPC answers `NOT_PERSONAL` | `duduclaw-gateway/src/isolation_lifecycle.rs:285`, `handlers/isolation_employee_rpc.rs:815-819` |
| In one apply.db `BEGIN IMMEDIATE` tx: `rename(agents/<label>, isolation-quarantine/<id hex>-<label>)`, chmod 0700, set `receipt.disabled = true`, delete the `employee_dir_markers` row. After commit the users.db mirror and bindings are removed (step 2, may fail) | `isolation_lifecycle.rs:273-304`, `:321-361` |
| Target check and `rename` are separate steps (TOCTOU); if `agents/<label>` is absent, `quarantine_agent_dir` returns `Ok` without moving anything | `isolation_lifecycle.rs:343-348`, `:324-325` |
| No digest and no security audit event. users.db gets `isolation.personal_agent_removed` with a NULL actor | `handlers/isolation_employee_rpc.rs:810-821`, `duduclaw-auth/src/db.rs:1819-1823` |
| The real revocation of most authorities today is `bump_acl_generation` inside `remove_personal_agent_in_transaction` (tombstone step 2). `record_personal_mirror` returns `Ok(false)` without a bump when a mirror exists | `db.rs:1824`, `:1741`, `:1780` |
| Root side untouched: the spawnd entry stays `Phase::Ready`; `ReadBack` checks Ready, UID, `verify_locked` and quota, not root-workspace contents. `Action::Disable` only for Failed/Rejecting/Rejected | `duduclaw-spawnd/src/allocation.rs:80-98`, `duduclaw-migration/src/allocation.rs:94-103` |
| spawnd refuses a new allocation for a label with a non-Disabled entry; a lost spawnd journal fails replay by `operation_id` | `duduclaw-spawnd/src/allocation.rs:57`, `:122-126` |
| Lifecycle refuses `prior.disabled` before the root ReadBack; `recheck_with` compares the whole `EmployeeLifecycle` | `isolation_lifecycle.rs:153`, `:137-145`, `:76-87` |
| The apply path never revives a disabled row (`checked_tenant` → `Denied`). This invariant stays | `handlers/migration_authority_employee.rs:150` |
| A proposal may be staged for a tombstoned label (`check_marker` → `LABEL_TAKEN` only for live rows); its apply fails at the root | `handlers/isolation_employee_dirs.rs:112-134` |
| Many checks bind only `(employee, label, binding[, allocation])`, not a full lifecycle: `prepare` pins a fresh lifecycle (`GrantPolicy` TTL 30 min); background receipts and the delegation caller check; delegation `decide` (employee hex only); Google routes; C6 details pinned to `(EmployeeId, agent)` | `isolation_mcp/authority.rs:440-443`, `boot.rs:417`, `isolation_background.rs:312`, `:375`, `isolation_employee_delegation.rs:69`, `google_identity/isolation_routes.rs:85`, `isolation_connector_details.rs:426` |
| Owner-scoped routes come from `active_routes_for(Personal(owner))` (odoo, search, model as well as Google/Notion/GitHub) | `google_identity/isolation_routes.rs:99-105` |
| Label-keyed state outside `agents/<label>`: org.toml entry (`reports_to` drives token resolution; covered by the marker digest), memory-DB namespace `agent:<label>` (`working_state`, `beliefs`, `user_profile`, `decisions`, `memory`), sessions, tasks, cron, `website_credentials.allowed_agents`, `[db_sources]` agent grants | `isolation_employee_dirs.rs:89-95`, `goal_notify.rs:116`, `boot.rs:400-407` |
| `agents/<label>/agent.toml` `[channels.*] bot_token(_enc)` starts per-agent Discord/Telegram bots | `discord.rs:343`, `telegram.rs:226` |
| Owner eligibility: `owner_eligible`; a forced password change only blocks until someone completes it | `isolation_lifecycle.rs:449-455` |

**The main gap.** Tombstone keeps the employee id, the UID and the root receipt. If restore only flipped
`disabled` back, every pre-tombstone authority would validate again, and if tombstone step 2 failed no
epoch moved at all. Restore needs (a) an `incarnation` carried by every persisted authority (§3.1),
(b) an unconditional `acl_generation`/`policy_epoch` bump, (c) a seal over the tree and the label-keyed
state (§3.2, §3.3), and (d) owner identity checks strong enough that an admin cannot act as the owner.

## 2. Decisions (owner, 2026-11-08)

- **Q1 Who starts it.** The **owner** files the restore request in 我的帳號 → 我的個人 AI 員工. Admins
  cannot file one. No request field names an owner: the owner is always the immutable receipt principal.
- **Q2 Owner authentication.** The strongest enrolled factor: MFA if enrolled (passkey, then TOTP,
  then email code); for SSO-only users without MFA, a fresh Google re-auth with `auth_time` ≤ 5 min.
  Never the current session alone, never the password alone. A password-only user without MFA is
  asked to enroll MFA first.
- **Q3 Credential changes.** Refuse if the owner's password, email, Google link or MFA/passkey set
  changed since the tombstone by anyone other than the owner (compared with the tombstone snapshot,
  §3.2). No override in v1.81.
- **Q4 Owner-scoped connections.** Google/Notion/GitHub and every other `Personal(owner)` route are
  listed on the request card. The request digest covers the full route list and is re-checked at
  apply. Per-employee detail routes (database/website/channel) always need fresh admin provisioning.
- **Q5 Retention.** Restorable for 90 days after the tombstone, checked at request and at approval.
  After that the quarantine may be cleaned up; the cleanup tool ships with this feature (F6i).
- **Q6 Unsealed or oversize quarantine.** Not restorable. The tombstone UI tells the admin at
  tombstone time.
- **Q9 Work after restore.** Schedules, tasks and goals stay paused; the owner resumes each one.
  Approvals from before the tombstone must be re-confirmed.
- **Q10 When it takes effect.** When an **Admin approves** the owner's request (Admin + step-up). The
  restore executes inside the approval call.
- **Q7 Shared restore** and **Q8 adoption under a new id after a root retire (F6g)**: not asked.
  Deferred, pending owner confirmation (recommendation: defer both).

Derived rules: same label and same employee id only (the label is digest-covered and copied into the
root `Receipt.label`; renaming means a new proposal and a new id). Personal only. Same-id restore works
because nothing disables the root entry; a future root retire (`storage.destroy`) ends it (F6g).

## 3. Backend design

### 3.1 Incarnation and authority inventory [SEC] (H1)
- apply.db `confirmed_employees.incarnation INTEGER NOT NULL DEFAULT 0` (added in
  `ensure_employee_schema`, `migration_authority_employee.rs:122-133`, like `tenant_id`). Not in the
  root `Receipt` (`deny_unknown_fields`, root-owned). Only restore writes it, only by +1;
  `save_confirmed_receipt` preserves it on conflict and writes 0 for a new id.
- `EmployeeLifecycle.incarnation: u32`, read in `resolve` from the same row read.
- Every persisted authority carries the incarnation and every check compares it with a fresh
  lifecycle. Restore's users.db tx also bumps `acl_generation` and `policy_epoch` unconditionally
  (H1a, §3.5). The table is the complete list; a new authority kind must add a row and a test.

| # | Authority | Binding added | Test |
|---|---|---|---|
| A1 | Lifecycle pins (`recheck_with`) | `incarnation` field | `lifecycle_incarnation_change_is_stale` |
| A2 | `GrantPolicy` / `RuntimeAuthorization` / `PreparedAuthority` (`authority.rs:440`, TTL 30 min) | incarnation in the policy input; `prepare` refuses a mismatch | `prior_incarnation_grant_policy_refused` |
| A3 | `RecordAuthority` and the background caller check (`isolation_background.rs:312`, `:375`) | `caller_incarnation` | `prior_incarnation_record_authority_refused` |
| A4 | Personal delegation `Gate` payload and `decide` (`personal_delegation.rs:102-122`, `isolation_employee_delegation.rs:69`) | `source_incarnation` in the payload and the `decide` check | `delegation_card_from_prior_incarnation_never_binds` |
| A5 | Google route selections (`isolation_routes.rs:85`) | `(employee, incarnation)` | `prior_incarnation_google_selection_refused` |
| A6 | C6 detail pins (`isolation_connector_details.rs:426`) | `(EmployeeId, incarnation, agent)`; plus revocation and fences (§3.2) | `prior_incarnation_detail_pin_refused` |
| A7 | users.db mirror and bindings | `acl_generation` + `policy_epoch` bump in restore | `restore_bumps_acl_generation_and_policy_epoch` |
| A8 | Other `ApprovalBroker` approvals and `ActionGrant`s naming the employee | invalidated at tombstone; payload carries incarnation | `tombstone_invalidates_employee_approvals` |
| A9 | Cron/goal/task rows with pre-tombstone approvals | resume requires a fresh approval if the approval's incarnation is older (Q9) | `resume_requires_reapproval_after_restore` |
| A10 | Sessions | session rows carry incarnation; an older session is never resumed | `prior_incarnation_session_not_resumed` |
| A11 | `DUDUCLAW_AGENT_TOKEN` and per-agent channel bots | live lifecycle required at use (inferred, verify the token's HMAC inputs) and §3.3 | `agent_token_refused_while_tombstoned` |
| A12 | In-memory registry entry and employee-keyed provider caches | rescan/evict at tombstone and at restore (L3) | `tombstone_evicts_registry_and_provider_caches` |

### 3.2 Sealing at tombstone (changes to `tombstone_personal_employee`) [SEC]
1. **Move** inside the existing apply.db tx with `renameat2(RENAME_NOREPLACE)` from held parent dir
   fds. Quarantine path and row are keyed by `(employee_hex, incarnation)`:
   `isolation-quarantine/<hex>-<incarnation>-<label>` (M4). If `agents/<label>` is absent, record
   seal `absent` (M2). Before the move, strip `[channels.*]` sections from `agent.toml` (H5; the
   stripped channel names are recorded) and the label-keyed grants (§3.3). The row starts as
   `seal = pending`. Commit.
2. **Seal outside the lock** (M1): walk the quarantine through a held directory fd (`openat` +
   `O_NOFOLLOW`, root `(dev, ino)` checked), compute the content digest and a stat manifest
   (`ino`, `size`, `ctime_ns` per entry). Manifest fields (M2): raw path bytes, type, mode, uid, gid,
   size, `nlink`, SHA-256 for regular files, `readlink` bytes for symlinks; entries sorted by raw bytes;
   every field length-prefixed (the `put` framing of `isolation_employee_dirs.rs:31-36`, domain tag
   `duduclaw-quarantine-v1`); the root mode is included. `seal = invalid` for sockets, FIFOs, devices,
   regular files with `nlink > 1`, entries with a foreign uid, or any xattr/ACL (v1.81 trees have none;
   verify on the Linux box). Bounds: ≤ 65 536 entries, ≤ 1 GiB, depth ≤ 16, else `oversize`. The org.toml
   entry for the label is added as a synthetic `org:<label>` entry (H4). A short tx writes
   `sealed | oversize | invalid` with the digest and stat manifest. A failed seal never blocks the
   tombstone, but the RPC returns the seal state so the UI can say "cannot be restored" (Q6).
3. **Owner snapshot** (H3), in the same row: `credential_epoch`, email, Google `sub`, a digest of the
   MFA/passkey set, and the contact address for notices.
4. **Fences** (H2), after commit and before the mirror removal: revoke every catalog route whose C6
   detail row names this employee hex (`isolation_connector_lifecycle::revoke_with`, `:157-166`).
   Each route that returns `Unavailable` or `MaterialPending` is recorded in the row's
   `pending_fences` (route id, state). Invalidate approvals naming the employee (A4, A8; the exact
   broker call, `approval/mod.rs:260`, `:357`, inferred, verify). Pause label-keyed tasks, goals and
   cron with reason `employee_tombstoned`. Record the store high-water marks (§3.3). Rescan the
   registry and evict employee-keyed provider caches (L3).
5. **Audit** `isolation.employee_tombstoned` through `security_autopilot::audit_and_emit` (pattern of
   `isolation_connector_lifecycle.rs:208-212`): actor, label, employee hex, incarnation, seal state,
   digest prefix, routes revoked, pending fences, approvals invalidated (M7).

apply.db row: `employee_quarantine(employee_hex, incarnation, label, owner_user_id, quarantined_at, dev,
ino, digest, stat_manifest, seal, owner_snapshot, pending_fences, stripped, store_marks,
restored_by_request, PRIMARY KEY (employee_hex, incarnation))`, `seal ∈ {pending, sealed, oversize,
invalid, absent, needs_reseal, restored, purged}`.

### 3.3 Label-keyed state outside the tree [SEC] (H4, H5)

| Store | Rule at tombstone | At restore |
|---|---|---|
| org.toml entry | in the digest (`org:<label>`) | the live entry must equal it, else `ORG_CHANGED` |
| memory DB `agent:<label>`, sessions, tasks, cron | writer gate: writes for a label whose lifecycle is not live are refused; high-water mark (row count, max rowid, max `updated_at`) recorded as `store_marks` | marks must match, else `STORES_CHANGED` |
| `website_credentials.allowed_agents`, `[db_sources]` agent grants | stripped, list kept in `stripped` | listed on the card as needing admin re-grant |
| `agent.toml [channels.*]` bot tokens | stripped before the seal | protected-mode bot startup also requires a live lifecycle **and** an active catalog route (gate, tested) |

### 3.4 Restore requests and state machine [SEC]
apply.db `employee_restores(request_id PK, employee_hex, incarnation, label, owner_user_id, digest,
route_digest, routes_shown, created_at, expires_at, state, admin_user_id, decided_at, failure)`, partial
unique index: one open request per employee (`state IN ('requested','applying')`). States:
`requested → applying → restored | failed`; `requested → cancelled | denied | expired`.
`expires_at = min(created_at + 72 h, quarantined_at + 90 d)`.

**Owner request** (`me.isolation.request_restore`, caller == receipt owner, Q2 factor proof). One
apply.db tx checks, and the admin approval re-runs all of them:
- the row is disabled with this employee id and incarnation; the principal is `Personal`;
- the quarantine row is `sealed` (not oversize, invalid, absent, pending, needs_reseal) and within 90
  days (Q5, Q6);
- the tombstone finished: users.db mirror absent and no `isolation_personal_agent_removals` row (the
  precheck retries the removal once, else `TOMBSTONE_INCOMPLETE`) (H1a);
- `pending_fences` is empty and no active route has a details row naming this employee hex; the
  precheck retries each fence first, `MaterialPending` gets a material-drop retry (H2);
- no `employee_dir_markers` row, no open Employee proposal (`label_pending`,
  `isolation_employee_rpc.rs:711`), `agents/<label>` absent;
- `owner_eligible`, and the owner snapshot matches (H3, Q3): any difference refuses unless every change
  since the tombstone has a log entry with actor == owner and an owner-authenticated session. Whether
  users.db records the actor per credential change today is unverified; if not, F6d adds a
  `credential_changes(user_id, field, actor_user_id, owner_authenticated, at)` log written by every
  mutation path, and a change without an entry refuses;
- Personal-employee quota and seat limits (L2);
- the root precheck passes.

The request stores `digest` and `route_digest`: a digest over `request_id`, employee, incarnation,
quarantine digest, and the full ordered route list (`active_routes_for(Personal(owner))` plus the
SharedHome fallback: route id, provider, account label, generation) (M5). Notices go to all Admins and
to the owner's tombstone-time contact address (H3).

**Root precheck** (`lifecycle::restore_precheck`): the `resolve` steps without the `prior.disabled`
gate; Applied proposal found, `spec.binding == prior.principal_binding`, `ReadBack` returns a
non-disabled receipt with the same employee, UID, label, quota and principal, and no live attachments
or processes for the employee UID (M8; which spawnd call lists them is inferred, verify; if none
exists, the residual is documented). Root-workspace contents are not covered (the employee UID can write
them) and this is stated. A lost or reset spawnd journal fails as `ROOT_UNAVAILABLE`.

**Limits** (L1): one open request per employee; ≤ 3 requests per employee per 30 days (cancelled,
denied and expired count); ≤ 5 admin notices per owner per day; failed factor checks count toward the
owner's login lockout counter.

**Admin decision** (`isolation.employee_restore_decide`, Admin + step-up via `authorize_step_up`,
`approve | deny`). Deny sets `denied`. Approve runs §3.5 inside the call (Q10).

### 3.5 Apply (inside the admin approval) [SEC]
Lock order: apply.db (`BEGIN IMMEDIATE`) before users.db, never the reverse (M6). F6d audits every path
that takes both: `save_confirmed_receipt`, tombstone, restore apply, recovery.
1. **Verify in quarantine, outside the lock** (M1): re-walk through a held fd, recompute the digest and
   a fresh stat manifest; the digest must equal the sealed one, else `failed: integrity` and the
   quarantine becomes `invalid`.
2. **tx1** (short): re-run every request check (§3.4) including the 90-day limit; recompute the route
   list digest and refuse `ROUTES_CHANGED` on a mismatch (the owner files a new request); set
   `state = applying`, `admin_user_id`. Commit.
3. **tx2** (`BEGIN IMMEDIATE`, `synchronous=FULL`, held to the end):
   a. `renameat2(quarantine → agents/<label>, RENAME_NOREPLACE)` from the held parent fds; root
      `(dev, ino)` must match the row.
   b. Stat-only re-walk: every entry's `ino` and `ctime_ns` must equal the step-1 manifest (the root,
      whose ctime the rename changes, is compared by `(dev, ino)`). Mismatch: rename back, `failed:
      integrity`, quarantine `needs_reseal`, audit, stop. Re-check the route digest and store marks.
   c. users.db `restore_personal_mirror(home, label, owner, snapshot)` (new, `duduclaw-auth/src/db.rs`)
      in one tx: refuse if a mirror or a removals row exists; re-check `owner_eligible` and the
      snapshot; insert the mirror; bind the owner; bump `acl_generation` and `policy_epoch` (H1a).
   d. `UPDATE confirmed_employees SET receipt = <disabled=false>, incarnation = incarnation + 1 WHERE
      label = ? AND receipt = <old> AND incarnation = <old>` (exactly one row); re-insert the
      `employee_dir_markers` row (inferred, verify it is needed for a live row); quarantine row
      `restored`, `restored_by_request = request_id`; request `restored`. Commit.
4. After commit: `read_lifecycle` must return Ok with the new incarnation (else alert; stays fail
   closed); `registry.scan()`; evict provider caches (L3); audit; notify the owner (in-app and
   tombstone-time contact address) and the approving admin.

**Mutual exclusion.** While a request is `applying`, tombstone, propose, purge, cleanup and a second
restore for the label refuse with `RESTORE_IN_PROGRESS` (a tombstone replay would remove the mirror
written in 3c, `isolation_lifecycle.rs:286-303`).

### 3.6 Crash recovery (gateway boot, and before any restore, tombstone or cleanup on the label) (M6)
- `applying` and row still disabled: roll back. If `agents/<label>` has the recorded `(dev, ino)`,
  rename it back with NOREPLACE; the quarantine becomes `needs_reseal`. Remove the mirror with
  `remove_personal_mirror` (its "no repair path" contract at `db.rs:1787-1793` is amended to name F6
  recovery; it bumps `acl_generation`). Set `failed: interrupted`; the owner files a new request.
- `applying`, row live, and quarantine `restored_by_request == request_id`: roll forward to `restored`.
  Row live without that match: alert, `failed: inconsistent`, nothing else runs for the label.
- `needs_reseal`: before any new request, re-verify against the original sealed digest; match →
  `sealed`, mismatch → `invalid`.
- Every intermediate crash point fails closed: a mirror without a live row reads `Tombstoned`
  (`disabled` checked first); a directory under `agents/` with a disabled row runs nothing.

### 3.7 After restore
- **Old route ids never come back.** Detail routes were revoked and fenced; C4/C3 never reactivate a
  revoked id. Per-employee routes (database, website, channel bots, computer) need
  `isolation.connector_provision` again (new ids, new details pinned to the new incarnation).
- **Owner-scoped routes** stay as listed on the request; the route digest guarantees the admin
  approved the same list. Old selections are dead (A5).
- **Work items** (Q9): tasks, goals and cron stay paused; the owner resumes each in 我的個人 AI 員工;
  a resume whose approval predates the incarnation needs a fresh approval (A9).

### 3.8 Quarantine cleanup (Q5) and audit (M7)
`isolation.quarantine_purge` (Admin + step-up): only rows older than 90 days, never with an open request;
removes the tree through held fds (no symlink following), sets `purged`. The root entry stays Ready, so
the label stays blocked until a root retire exists (F6g). Every transition goes through `audit_and_emit`
(actor, `request_id`, employee hex, incarnation, digest prefix): tombstone, seal result, fence retry,
request, cancel, deny, expire, approve, apply failure, recovery rollback / roll-forward / inconsistent,
reseal result, purge.

## 4. UI (Calm Glass, zh-TW wording; extends the F5 flow)
- **Admin → 受保護模式 → AI 員工 → 註銷 dialog**: shows the estimated data size before confirming
  (「超過 1 GiB 的資料之後無法還原」). After the tombstone the result says whether it can be restored;
  on a failed or oversize seal: 「此員工的資料無法完整封存，之後無法還原」 (Q6).
- **Owner → 我的帳號 → 我的個人 AI 員工**: an 已註銷 section lists own tombstoned employees with
  可申請還原（至 YYYY-MM-DD） or the reason it cannot be. 申請還原 opens a card: what comes back; the
  owner-scoped connections that become usable again (provider, account); what needs admin
  re-provisioning (database/website/channel connections, website and database grants); work items
  that stay paused. Confirming asks for the strongest factor (Q2). Then 等待管理員核准 with 取消.
- **Admin → 受保護模式 → AI 員工**: the 已註銷 row shows 註銷時間, 主人, a 隔離區狀態 badge (可還原 /
  無法還原：封存失敗或超過大小 / 主機資源已釋放 / 已超過 90 天保留期限) and, when a request is open,
  主人申請還原 with the route list, then 核准 / 拒絕 (step-up). 清除隔離區 appears after 90 days. The
  紀錄 tab adds every §3.8 event.
- Refusals in plain words: 原主人目前無法使用帳號 / 主人的登入方式在註銷後被他人變更，無法還原 / 已有同名員工
  或提案 / 隔離區資料與註銷時不一致 / 主機資源已釋放 / 已超過保留期限 / 主人的連線已變更，請主人重新申請 /
  另一個還原正在進行 / 請先設定兩步驟驗證.
- Not a new page, so no `newIn` nav badge.

## 5. Threat model

| Actor | Attack | Control |
|---|---|---|
| Former owner (offboarded, suspended) | Requests the data back | `owner_eligible` at request, at approval (tx1) and inside the mirror tx (3c); the lifecycle re-checks on every use |
| Other staff | Requests for someone else, or sees the card | `me.isolation.request_restore` requires caller == receipt owner; listings filtered to the caller; not-owned ids answer `NOT_FOUND` |
| Admin hands the data to someone else | Picks a different owner or label | No owner or label input; owner from the root receipt principal, label from the digest-covered proposal; no new id |
| Admin restores without the owner | Files or applies a request alone | Admins have no request RPC; approve only acts on an owner-filed request bound to the owner's factor proof |
| Admin acts as the owner | Resets the owner's password, email, Google link or MFA, logs in, files, approves | Q3 snapshot refusal (H3); Q2 needs MFA or a fresh Google re-auth, not a password; notices also go to the tombstone-time contact address. Residual: a Google Workspace super-admin who can re-auth as the owner at Google is trusted |
| Admin approves a changed scope | Owner-scoped routes added after the request | Route digest re-checked in tx1 and tx2 (M5) |
| Admin restores tampered data | Edits the quarantine while held | Digest verified in quarantine, stat re-walk after the move, `(dev, ino)` check, move undone on mismatch. **Limitation (M3):** the seal row lives in apply.db with the same uid as the quarantine, so a host shell as that uid can rewrite both; the seal stops RPC-level actors and corruption only. Optional F6j: spawnd (root) countersigns the digest in a root-owned record (not the `Receipt`) |
| AI employee | Calls restore, or revives itself | No MCP tool; dashboard-only `isolation.*`/`me.*` RPCs; `user_id == "system"` refused (`isolation_employee_rpc.rs:182`); methods added to the not-listed/Admin scope table; a tombstoned employee cannot run |
| Holder of an old capability, selection, card or session | Waits for the restore | Authority inventory A1–A12 (§3.1); `acl_generation`/`policy_epoch` bump; fences (H2) |
| Writer to label-keyed stores, or a revived channel bot | Plants memory, tasks or grants while tombstoned; a bot token in `agent.toml` starts after restore | Writer gate and store marks; grants and `[channels.*]` stripped; bot startup gate (§3.3, H4, H5) |
| Local same-uid racer | Symlink or directory swap, pre-creating `agents/<label>` | `renameat2(NOREPLACE)` from held fds; `O_NOFOLLOW` walk; root `(dev, ino)`; ctime re-walk; editing `agents/<label>` after restore stays out of scope (same as today) |
| Concurrent actions, flooding | Tombstone, propose, purge during a restore; many requests or notices | `RESTORE_IN_PROGRESS` plus `BEGIN IMMEDIATE`; one open request per employee; L1 caps; factor failures count toward lockout |

## 6. Task breakdown
- **F6a [SEC] seal and fence at tombstone.** `isolation_lifecycle.rs` (held-fd NOREPLACE, row keyed by
  incarnation, owner snapshot, pause work items), new `isolation_quarantine.rs` (manifest, stat
  manifest, seal), `isolation_connector_lifecycle.rs` (`revoke_employee_detail_routes`, pending fences),
  `personal_delegation.rs` + `approval/mod.rs` (invalidate by employee), registry/cache eviction, audit.
  Tests (`tombstone_` prefix): `seals_manifest_outside_lock`, `oversize_still_tombstones_and_reports`,
  `absent_dir_seals_absent`, `noreplace_refuses_existing_target`, `records_pending_fence_on_unavailable`,
  `revokes_employee_detail_routes_only`, `snapshots_owner_identity`, `strips_channel_sections_and_grants`,
  `audited`; plus `manifest_rejects_hardlink_foreign_uid_xattr`, `manifest_orders_raw_bytes`,
  `second_tombstone_after_restore_uses_new_key`.
- **F6b [SEC] incarnation and authority inventory.** `migration_authority_employee.rs` (schema,
  preserve on conflict), `isolation_lifecycle.rs`, `isolation_mcp/authority.rs`,
  `isolation_background.rs`, `personal_delegation.rs`, `isolation_employee_delegation.rs`,
  `google_identity/isolation_routes.rs`, `isolation_connector_details.rs`, sessions. Tests: one per §3.1
  row (A1–A12) plus `apply_preserves_incarnation`, `legacy_row_reads_incarnation_zero`.
- **F6c [SEC] restore engine.** New `isolation_restore.rs` (journal, checks, `restore_precheck`, apply,
  recovery at boot); `duduclaw-auth/src/db.rs` (`restore_personal_mirror`, amended
  `remove_personal_mirror` contract, `credential_changes` log if missing). Tests (`restore_` prefix):
  `same_id_same_owner_ok`, `refuses_ineligible_owner` (3 points), `refuses_credential_change_by_non_owner`,
  `allows_owner_own_credential_change`, `refuses_incomplete_tombstone`, `refuses_pending_fence_and_retries`,
  `refuses_after_90_days`, `refuses_unsealed_or_oversize`, `rechecks_quota`,
  `refuses_open_proposal_or_marker`, `refuses_existing_agents_dir`, `integrity_mismatch_in_quarantine`,
  `ctime_change_after_move_moves_back`, `routes_changed_refused`, `store_marks_changed_refused`,
  `org_entry_changed_refused`, `root_readback_mismatch_refused`, `root_live_process_refused`,
  `lost_spawnd_journal_root_unavailable`, `crash_after_rename_rolls_back_needs_reseal`,
  `crash_after_mirror_rolls_back`, `crash_after_commit_rolls_forward_by_request`,
  `live_row_without_request_match_alerts`, `every_transition_audited`; plus
  `tombstone_refused_while_restore_applying`, `apply_path_still_never_revives_disabled_row`.
- **F6d [SEC] RPCs** in `handlers/isolation_employee_rpc.rs` (METHODS `:61-62`) + new child module
  `isolation_employee_restore.rs`: owner `me.isolation.list_restorable|request_restore|cancel_restore`
  (request needs the Q2 factor); Admin + step-up `isolation.employee_restore_decide` (`approve|deny`),
  `isolation.employee_restore_cancel`, `isolation.quarantine_purge` (F6i); `employee_list` adds quarantine
  and request state; `prepare_propose`/`stage_prepared` refuse during an open restore. Tests:
  `request_only_by_owner`, `request_requires_strongest_factor` (MFA user with password only refused;
  SSO user with `auth_time` > 5 min refused), `factor_failures_count_toward_lockout`,
  `decide_requires_admin_step_up`, `admin_cannot_file_request`, `request_rate_limits_per_employee`,
  `owner_notice_cap`, `request_expires`, `cancel_by_owner_or_admin`, `restore_rpcs_not_listed_to_ai`.
- **F6e UI.** Protected-mode page, the 我的帳號 tab, `web/src/lib/api.ts`, i18n ×3. Design canvas
  first (`feedback_logic_first_then_design`).
- **F6f.** G2 `iso_personal_chain.rs`: `iso_root_e2e_tombstone_restore_same_uid` (root ReadBack
  unchanged, old capability refused, new one works, channel bot not started); CHANGELOG Added; F5 doc
  decision 4 marked done; `docs/features` isolation page.
- **F6g (deferred, Q8).** Adopt under a new id once a root retire exists: an Employee proposal for
  `Personal(same owner)` with a digest-covered `adopt_quarantine = <hex>:<incarnation>:<digest>`,
  reseal under the new incarnation, never carry the root workspace, single-use (row `adopted`).
- **F6h [SEC] store and channel gates (H4, H5).** Writer gate for memory DB `agent:<label>`, sessions,
  tasks and cron; protected-mode per-agent bot startup gate. Tests: one per writer path
  (`tombstoned_label_memory_write_refused`, `…_session_write_refused`, `…_task_write_refused`,
  `…_cron_write_refused`), `channel_bot_needs_live_lifecycle_and_route`.
- **F6i cleanup tool (Q5).** Tests: `purge_refused_before_90_days`, `purge_refused_with_open_request`,
  `purge_no_symlink_follow`, `purge_audited`. **F6j (optional, M3)**: spawnd countersign of the seal
  digest, only if the owner puts host-shell admins in scope.

## 7. Items not decided by the owner
- Q7 Shared restore and Q8 adoption (F6g): deferred, pending owner confirmation (recommendation: defer
  both; a later Shared restore would be Admin-only with tenant == apply.db pin == users.db tenant).
- Self-approval (new, from the Q1/Q10 split): when the owner is also an Admin, the design default is
  that they cannot approve their own request while another active Admin exists; as the only Admin they
  may, with step-up, audited as `self_approved`. Pending owner confirmation.
