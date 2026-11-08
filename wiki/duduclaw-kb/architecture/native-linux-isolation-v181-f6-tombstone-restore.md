# F6: tombstone, restore and adoption of protected employees (v1.81.0, design)

Status: design r4, 2026-11-08, not implemented. Code read at `feat/iso181-w12-20261108` @ 92faab9e2;
the root rows added in r4 were read at c6016a33f (paths under `crates/`).
F5 decision 4: restore a tombstoned employee to the same id. r2 folded in the security review of
2026-11-08 (baton run `20261108-191211-claude-18045-173`, GO with changes). r3 folded in the owner
decisions of the same day: the self-approval rule, Shared tombstone and restore (Q7) and adoption under
a new id (Q8, F6g) are all in v1.81 scope. r4 folds in the second round: the 7-day cooling rule stays
(R2), adoption carries no label-keyed state (N1), the 90-day label hold with an early purge (N3), and
**root retire is in v1.81 scope** (N2, §3.12, F6l). Review ids stay next to each rule: H1–H5 are in the
design before F6a–F6d start; M/L items land with the implementation, each with its §6 test. r3 rules
are tagged R1–R9, r4 rules R10–R15. [SEC] = Daybreak review before merge. Inferences are marked
"(inferred, verify)".

## 1. What exists today

| Fact | Where |
|---|---|
| Tombstone is Personal-only: a Shared row hits `let Some(Personal(owner)) … else Denied` and the RPC answers `NOT_PERSONAL` | `duduclaw-gateway/src/isolation_lifecycle.rs:285`, `handlers/isolation_employee_rpc.rs:841-851` |
| In one apply.db `BEGIN IMMEDIATE` tx: move `agents/<label>` to `isolation-quarantine/<id hex>-<label>` (0700), set `receipt.disabled = true`, delete the `employee_dir_markers` row; after commit the users.db mirror and all bindings go (step 2, may fail) | `isolation_lifecycle.rs:273-304` |
| Since 92faab9e2 the move is one descriptor-relative `renameat2(RENAME_NOREPLACE)` between pinned dirs (the r2 TOCTOU row is fixed); an absent `agents/<label>` still returns `Ok` with nothing moved | `isolation_lifecycle.rs:332-360`, `:340` |
| No digest, no security audit event; users.db logs `isolation.personal_agent_removed` with a NULL actor | `duduclaw-auth/src/db.rs:1819-1823` |
| Most authorities are really revoked by `bump_acl_generation` in `remove_personal_agent_in_transaction` (tombstone step 2); `record_personal_mirror` returns `Ok(false)` without a bump when a mirror exists | `db.rs:1800-1824`, `:1741`, `:1780` |
| Root side untouched: the spawnd entry stays `Phase::Ready`; `ReadBack` checks Ready, UID, `verify_locked`, quota, not workspace contents. `Action::Disable` (phase `Disabling`, `registry.retire`, `recover_locked` = lock + kill strays + no-process readback, `storage.destroy`, `Disabled`) is allowed only for Failed/Rejecting/Rejected items, so no retire exists for an Applied employee | `duduclaw-spawnd/src/allocation.rs:80-99`, `accounts.rs:562-578`, `duduclaw-migration/src/allocation.rs:93-106` |
| spawnd refuses a new allocation for a label held by a non-Disabled entry; a lost spawnd journal fails replay by `operation_id`; journal ≤ 4096 entries, none ever removed | `duduclaw-spawnd/src/allocation.rs:57`, `:110`, `:122-126` |
| UIDs are never recycled: `retire` keeps the mapping, `reserve` skips every registered UID; range 70 000–99 999 | `registry.rs:269-275`, `:105-113`, `storage.rs:104`, `duduclaw-isolation/src/ids.rs:18-19` |
| Every non-ReadBack allocation action is refused `NotReady` while **any** run is live on the host; allocation and runs share one lifecycle worker. Run units are transient per run (`duduclaw-run-<run>.service`), none per employee; accounts are `nologin`, expired, linger off | `host.rs:449`, `allocation.rs:1`, `systemd.rs:30`, `accounts.rs:548-559` |
| `storage.destroy` unlinks the loop image (no overwrite) and refuses an active lease; it is journaled with an operation id and resumable | `storage/adapter.rs:189-214`, `duduclaw-isolation-storage/src/linux.rs:259-286` |
| The allocation `Response` carries only `receipt: Option`; every refusal and a 4 s worker timeout reply `None`, an invalid request gets no reply | `duduclaw-migration/src/allocation.rs:29`, `duduclaw-spawnd/src/allocation.rs:129-141` |
| Confirmation items have no exit from `Applied`; a failed apply runs `disable_and_read_back`; `reject` stays `Rejecting` until compensation succeeds. spawnd's root audit `Event` set has no allocation events | `duduclaw-isolation/src/migration.rs:218-232`, `confirmation.rs:345-361`, `:395-421`, `duduclaw-spawnd/src/audit.rs:8` |
| The apply path never revives a disabled row of the same id, but accepts a **new id** over a disabled row of another id, even with a different tenant (`checked_tenant(dead, receipt(2), "tenant-b")` is Ok); `confirmed_employees.label` is the primary key | `handlers/migration_authority_employee.rs:141-155`, test `:180`, `:122` |
| A proposal may be staged for a tombstoned label (`check_marker` → `LABEL_TAKEN` only for live rows); today its apply fails only because spawnd still holds the label | `handlers/isolation_employee_dirs.rs:194-215` |
| Marker digest covers only `ALLOWED_TOP` (SOUL.md, CONTRACT.toml, agent.toml, SKILLS/, `.claude/settings.json`) plus the org entry, domain `duduclaw-employee-dir-v2`, label in the hash; anything else refuses (`FOREIGN_DIR`) | `isolation_employee_dirs.rs:41`, `:136-175` |
| `agent.toml [agent] name` equals the label (registry and delegation resolve by it) | `handlers/agents_create.rs:95-115` |
| Many checks bind only `(employee, label, binding[, allocation])`: `prepare` (TTL 30 min), background receipts, delegation `decide`, Google routes, C6 detail pins | `isolation_mcp/authority.rs:440-443`, `boot.rs:417`, `isolation_background.rs:312`, `:375`, `isolation_employee_delegation.rs:69`, `google_identity/isolation_routes.rs:85`, `isolation_connector_details.rs:426` |
| Routes: `Personal` binds a person; `SharedTenant`/`SharedHome` bind `Shared(tenant)`, i.e. the whole tenant, not one employee; `active_routes_for(binding)` lists them | `isolation_connectors.rs:77-100`, `:408` |
| Shared members are users.db `user_agent_bindings(user_id, agent_name, access_level, is_primary)` rows; a Shared lifecycle requires tenant == pin and no Personal mirror | `db.rs:102-122`, `isolation_lifecycle.rs:410-422` |
| Label-keyed state outside `agents/<label>`: org.toml entry, memory-DB namespace `agent:<label>`, sessions, tasks, cron, `website_credentials.allowed_agents`, `[db_sources]` grants; `agent.toml [channels.*] bot_token(_enc)` starts per-agent bots | `isolation_employee_dirs.rs:160-171`, `boot.rs:400-407`, `discord.rs:343`, `telegram.rs:226` |
| Whole-home backup restore is refused in protected mode | `backup_restore.rs:515-533` |

**Gaps.** Tombstone keeps the id, UID and root receipt, so flipping `disabled` back would revive every
old authority: restore needs an `incarnation` in every authority (§3.1), an unconditional epoch bump, a
seal over tree and label-keyed state (§3.2, §3.3) and identity checks an admin cannot fake. **R7 (new in
r3):** once spawnd no longer holds a label (lost journal), any new id can be applied over a tombstoned
label and inherit another person's or tenant's `agent:<label>` memory, sessions, tasks and grants; §3.10
step 7 closes it for every proposal. **N2 (r4):** nothing frees a tombstoned employee's root allocation
(UID account, loop image, journal entry holding the label), so a purged label stays blocked at spawnd and
the disk is never returned; §3.12 adds the root retire.

## 2. Decisions (owner, 2026-11-08)

- **Q1 Who starts it.** Personal: the **owner** files in 我的帳號 → 我的個人 AI 員工; no request field
  names an owner (always the immutable receipt principal). Shared: an **Admin** files (Q7).
- **Q2 Owner authentication.** Strongest enrolled factor: passkey, then TOTP, then email code; SSO-only
  users without MFA: fresh Google re-auth with `auth_time` ≤ 5 min. Never the session or password alone;
  a password-only user enrolls MFA first.
- **Q3 Credential changes.** Refuse if the owner's password, email, Google link or MFA/passkey set
  changed since the tombstone by anyone other than the owner (tombstone snapshot, §3.2). No override.
- **Q4 Routes.** Every route the employee would use again is listed on the card; the request digest covers
  the full list and is re-checked at apply. Per-employee detail routes always need fresh provisioning.
- **Q5 Retention.** 90 days after the tombstone, checked at request, approval and apply; cleanup ships (F6i).
- **Q6 Unsealed or oversize quarantine.** Not restorable or adoptable; the tombstone UI says so.
- **Q9 Work after restore.** Schedules, tasks and goals stay paused; pre-tombstone approvals re-confirmed.
- **Q10 When it takes effect.** When the approving Admin approves (Admin + step-up), inside that call.
- **SA Self-approval (R1).** An approver is never the requester: a Personal owner who is also an Admin
  cannot approve their own request, and a Shared requester cannot approve their own, while any other
  active Admin exists. If the requester is the only active Admin, self-approval is allowed with step-up
  and is audited as `self_approved`.
- **Q7 Shared tombstone and restore: in scope** (§3.9). **Q8 Adoption under a new id when the old root
  allocation is gone or cannot be reused: in scope** (F6g, §3.10).

Second round (owner, 2026-11-08, r4):
- **R2 Cooling rule: 7 days.** An Admin demoted or disabled within 7 days blocks self-approval (§3.4).
- **N1** Adoption carries no label-keyed state: memory, sessions, tasks and cron stay sealed under the
  quarantine row and are purged with it (§3.3).
- **N3 / R7 Label hold.** No new employee on a tombstoned label for 90 days unless the row is adopted or
  purged; an Admin may purge early with step-up, deleting the quarantine **and** the label's stores (R10,
  §3.8). The label is then free at the gateway, and at spawnd once the allocation is retired (§3.12).
- **N2 Root retire: in v1.81 scope** (R11–R15, §3.12, F6l). Retire wipes the root workspace (R13).

Derived rules: restore keeps the label and the employee id (the label is digest-covered and copied
into `Receipt.label`). A different label or id is only possible through adoption (§3.10), which is a new
employee. Same-id restore works because nothing touches the root entry until retire; retire ends every
restore and adoption path for that row.

## 3. Backend design

### 3.1 Incarnation and authority inventory [SEC] (H1)
- apply.db `confirmed_employees.incarnation INTEGER NOT NULL DEFAULT 0` (added in
  `ensure_employee_schema`, like `tenant_id`). Not in the root `Receipt` (`deny_unknown_fields`). Only
  restore writes it, only by +1; `save_confirmed_receipt` preserves it on conflict for the same id and
  writes 0 for a new id (adoption therefore starts at 0 under a new employee id).
- `EmployeeLifecycle.incarnation: u32`, read in `resolve` from the same row read.
- Every persisted authority carries **employee id and incarnation**; every check compares both with a
  fresh lifecycle. Restore's users.db tx bumps `acl_generation` and `policy_epoch` unconditionally (H1a).
  A new authority kind must add a row and a test.

| # | Authority | Binding added | Test |
|---|---|---|---|
| A1 | Lifecycle pins (`recheck_with`) | `incarnation` field | `lifecycle_incarnation_change_is_stale` |
| A2 | `GrantPolicy` / `RuntimeAuthorization` / `PreparedAuthority` | incarnation in the policy input; `prepare` refuses a mismatch | `prior_incarnation_grant_policy_refused` |
| A3 | `RecordAuthority`, background caller check | `caller_incarnation` | `prior_incarnation_record_authority_refused` |
| A4 | Personal delegation `Gate` payload and `decide` | `source_incarnation` in both | `delegation_card_from_prior_incarnation_never_binds` |
| A5 | Route selections (Google and Shared tenant routes) | `(employee, incarnation)` | `prior_incarnation_selection_refused` |
| A6 | C6 detail pins | `(EmployeeId, incarnation, agent)`; revocation and fences (§3.2) | `prior_incarnation_detail_pin_refused` |
| A7 | users.db mirror, member bindings | `acl_generation` + `policy_epoch` bump at tombstone, restore and adoption | `restore_bumps_acl_generation_and_policy_epoch` |
| A8 | `ApprovalBroker` approvals, `ActionGrant`s naming the employee | invalidated at tombstone; payload carries incarnation | `tombstone_invalidates_employee_approvals` |
| A9 | Cron/goal/task rows with approvals | resume needs a fresh approval if older (Q9) | `resume_requires_reapproval_after_restore` |
| A10 | Sessions | rows carry incarnation; never resumed across one | `prior_incarnation_session_not_resumed` |
| A11 | `DUDUCLAW_AGENT_TOKEN`, per-agent channel bots | live lifecycle at use; token HMAC must cover employee id + incarnation, not only the label (inferred, verify inputs) | `agent_token_refused_while_tombstoned`, `agent_token_of_old_id_refused_on_reused_label` |
| A12 | Registry entry, employee-keyed provider caches | rescan/evict at tombstone, restore, adoption (L3) | `tombstone_evicts_registry_and_provider_caches` |
| A13 | Label-only state (memory `agent:<label>`, sessions, tasks, cron, grants, org entry) under a new id on the same label (R7) | re-keyed under the row (adoption) or purged before any new id goes live on the label (§3.10) | `new_id_on_quarantined_label_never_sees_old_state` |

### 3.2 Sealing at tombstone (one `tombstone_employee`, Personal and Shared) [SEC]
1. **Move** inside the apply.db tx (already NOREPLACE from held fds). Path and row keyed by
   `(employee_hex, incarnation)`: `isolation-quarantine/<hex>-<incarnation>-<label>` (M4). Absent tree:
   seal `absent` (M2). Before the move strip `[channels.*]` from `agent.toml` (H5, names recorded) and
   the label-keyed grants (§3.3). Row starts `seal = pending`. Commit.
2. **Seal outside the lock** (M1): walk through a held fd (`openat` + `O_NOFOLLOW`, root `(dev, ino)`
   checked); content digest plus stat manifest (`ino`, `size`, `ctime_ns`). Manifest fields (M2): raw
   path bytes, type, mode, uid, gid, size, `nlink`, SHA-256 per regular file, `readlink` bytes; sorted by
   raw bytes; length-prefixed (`put` framing, domain `duduclaw-quarantine-v1`); root mode included.
   `invalid` for sockets, FIFOs, devices, `nlink > 1`, foreign uid, any xattr/ACL (verify on the Linux
   box). Bounds ≤ 65 536 entries, ≤ 1 GiB, depth ≤ 16, else `oversize`. The org entry is a synthetic
   `org:<label>` entry (H4). Short tx writes the result. A failed seal never blocks the tombstone; the RPC
   returns the seal state (Q6).
3. **Principal snapshot.** Personal (H3): `credential_epoch`, email, Google `sub`, MFA/passkey-set
   digest, notice address. Shared (R4): the pinned `tenant_id` (legacy unpinned rows refuse tombstone,
   as `resolve` already refuses them) and `members` = every binding `(user_id, access_level, is_primary)`.
4. **Fences** (H2), after commit and before step 5: revoke every catalog route whose C6 detail row names
   this employee hex; `Unavailable`/`MaterialPending` go to `pending_fences`. Tenant-wide
   `SharedTenant`/`SharedHome` routes are **not** revoked (other employees use them); the employee's
   selections and pins to them die by A5/A6. Invalidate approvals naming the employee (A4, A8; broker
   call at `approval/mod.rs:260`, `:357` inferred, verify). Pause label-keyed work with reason
   `employee_tombstoned`. Record store high-water marks (§3.3). Rescan registry, evict caches (L3).
5. **users.db**: Personal `remove_personal_mirror` (exists); Shared new
   `remove_shared_bindings_in_transaction` (delete all bindings of the agent, refuse if a Personal mirror
   exists, bump `acl_generation`). A failure leaves the employee unusable (row disabled) and is retried.
6. **Audit** `isolation.employee_tombstoned` via `security_autopilot::audit_and_emit`: actor, kind,
   label, employee hex, incarnation, seal state, digest prefix, routes revoked, pending fences (M7).

apply.db `employee_quarantine(employee_hex, incarnation, label, kind, owner_user_id NULL, tenant_id,
quarantined_at, dev, ino, digest, stat_manifest, seal, principal_snapshot, pending_fences, stripped,
store_marks, closed_by_request, adopted_by_employee NULL, root_item, root_state, retire_proposal_id NULL,
purged_at NULL, PRIMARY KEY (employee_hex, incarnation))`. `root_item` is the Applied allocation
`ConfirmationItem` and receipt copied at tombstone, so a retire can be staged after adoption replaced the
`confirmed_employees` row (R12). `root_state ∈ {allocated, retiring, retired, root_absent}`. No release
shipped employee tombstone, so there is nothing to backfill; rows left by dev builds read `invalid`.

### 3.3 Label-keyed state outside the tree [SEC] (H4, H5)

| Store | Rule at tombstone | At restore | At adoption (§3.10) |
|---|---|---|---|
| org.toml entry | in the digest (`org:<label>`) | live entry must equal it, else `ORG_CHANGED` | the proposal states the new entry |
| memory `agent:<label>`, sessions, tasks, cron | writer gate refuses writes for a non-live label; marks recorded | marks must match, else `STORES_CHANGED` | re-keyed under `quarantine:<hex>-<inc>` before the new id goes live, sealed, never carried, purged with the row (N1) |
| `website_credentials.allowed_agents`, `[db_sources]` grants | stripped, kept in `stripped` | listed for admin re-grant | listed for admin re-grant |
| `agent.toml [channels.*]` | stripped before the seal | bot startup needs live lifecycle **and** an active catalog route | same |

### 3.4 Requests, approval rule and state machine [SEC]
apply.db `employee_restores(request_id PK, kind ∈ {personal_restore, shared_restore, personal_adopt,
shared_adopt}, employee_hex, incarnation, label, target_label NULL, owner_user_id NULL, tenant_id,
requester_user_id, digest, route_digest, member_selection, created_at, expires_at, state,
approver_user_id, self_approved, proposal_id NULL, decided_at, failure)`; partial unique index: one open
request per quarantine row (`state IN ('requested','applying')`). `expires_at = min(created_at + 72 h,
quarantined_at + 90 d)`.

```
quarantine: pending → sealed | oversize | invalid | absent
            sealed → restoring → restored | needs_reseal          (§3.5, §3.9)
            sealed → adopting  → adopted  | needs_reseal          (§3.10)
            needs_reseal → sealed | invalid
            any state but pending/restoring/adopting/restored, no open request:
              → purging → purged   (≥ 90 d; or early < 90 d with Admin + step-up, R10)
request:    requested → applying → done | failed;   requested → cancelled | denied | expired
root_state (on the quarantine row, §3.12):
            allocated → retiring → retired | root_absent    only from purged or adopted
            retiring → allocated   only while spawnd still reports the entry Ready (retire refused)
spawnd entry (journal schema 3):
            Planned → Ready → Retiring → Retired;   Planned | Ready → Disabling → Disabled (compensation)
```
`restored` rows keep `root_state = allocated` (the employee is live again; a later tombstone starts a new
row under the next incarnation).

**Common checks**, run at request, again in the approval call (tx1) and in the apply tx: quarantine row
`sealed`, within 90 days, `root_state = allocated`; tombstone finished (users.db side removed, no
`isolation_personal_agent_removals` row; precheck retries once, else `TOMBSTONE_INCOMPLETE`, H1a);
`pending_fences` empty and no active route has a detail row naming the employee hex (retry first, H2); no
`employee_dir_markers` row, no open proposal (`label_pending`), `agents/<target>` absent; tenant ==
apply.db pin == users.db tenant; quota and seats (L2). **Personal adds:** caller == receipt owner with a
Q2 factor proof; `owner_eligible`; snapshot match (Q3: any difference refuses unless each change has a
log entry with actor == owner and an owner-authenticated session; F6d adds `credential_changes(user_id,
field, actor_user_id, owner_authenticated, at)` if users.db lacks it). **Shared adds:** §3.9.

**Approval rule (R1, SA).** `isolation.employee_restore_decide` (Admin + step-up, `approve | deny`) and
the adoption proposal's approval card (§3.10) both run, inside the users.db read of the apply tx:
- `approver != requester` (Personal: `approver != owner` too) while another **active Admin** exists:
  role Admin, status active, no pending forced password change, MFA enrolled (can pass step-up);
- otherwise (requester is the only active Admin) approve with a fresh step-up, `self_approved = 1`,
  audit `isolation.employee_restore_self_approved`, notices to every account that held Admin in the last
  30 days and (Personal) the tombstone-time notice address;
- **cooling rule (R2, owner decision, 7 days):** self-approval refuses while any other account lost
  Admin role or active status in the last 7 days (read from the audited role/status log, inside the
  same tx as the admin count).

**Root precheck** (`lifecycle::restore_precheck`): `resolve` without the `prior.disabled` gate; Applied
proposal found, `spec.binding == prior.principal_binding`, `ReadBack` gives a non-disabled receipt with
the same employee, UID, label, quota, principal; no live attachments or processes for the UID (M8: spawnd
has no such query today, so F6l returns the §3.12 step-4 result as `idle` in the schema-2 ReadBack).
Workspace contents are not covered (stated). Result classes (R5): `ok`; `ROOT_UNAVAILABLE` (transport
error, spawnd down: retry later, no adoption); `ROOT_GONE` (spawnd answers and holds no Ready entry for
the employee, e.g. lost or reset journal, or ReadBack mismatches UID, label, quota or principal: same-id
restore impossible, adoption offered); `ROOT_RETIRED` (`root_state` retired or retiring: nothing is
possible). Today the allocation `Response` cannot tell these apart (every refusal is `receipt: None`,
§1), so F6l adds Response schema 2 with `error: Option<ErrorCode>` (`NotFound` = no journal entry for the
operation, `NotReady`, `InvalidRequest`, `IsolationUnavailable`); a transport error or no reply stays
`ROOT_UNAVAILABLE`. `ROOT_GONE` needs an explicit `NotFound` or a ReadBack mismatch.

**Limits** (L1): one open request per quarantine row; ≤ 3 requests per row per 30 days (cancelled, denied,
expired count); ≤ 5 notices per owner (Personal) or per tenant (Shared) per day; failed factor checks count
toward the login lockout counter.

### 3.5 Same-id apply (inside the approval call) [SEC]
Lock order apply.db (`BEGIN IMMEDIATE`) before users.db, never the reverse (M6); F6d audits every path
that takes both (`save_confirmed_receipt`, tombstone, restore, adoption, recovery).
1. **Verify in quarantine, outside the lock** (M1): re-walk through a held fd, recompute the digest and a
   fresh stat manifest; mismatch → `failed: integrity`, quarantine `invalid`.
2. **tx1** (short): re-run every check including R1 and the 90-day limit; recompute the route digest
   (`ROUTES_CHANGED` → requester files again); state `applying`, approver, quarantine `restoring`. Commit.
3. **tx2** (`BEGIN IMMEDIATE`, `synchronous=FULL`, held to the end):
   a. `renameat2(quarantine → agents/<label>, NOREPLACE)` from held fds; root `(dev, ino)` must match.
   b. Stat-only re-walk: `ino`/`ctime_ns` equal the step-1 manifest (root by `(dev, ino)`). Mismatch:
      rename back, `failed: integrity`, `needs_reseal`, audit, stop. Re-check route digest and store marks.
   c. users.db in one tx: Personal `restore_personal_mirror(home, label, owner, snapshot)`; Shared
      `restore_shared_bindings(home, label, tenant, member_selection)` (§3.9). Both refuse if a mirror or
      binding exists, re-check eligibility, bump `acl_generation` and `policy_epoch` (H1a).
   d. `UPDATE confirmed_employees SET receipt = <disabled=false>, incarnation = incarnation + 1 WHERE
      label = ? AND receipt = <old> AND incarnation = <old>` (one row); re-insert the marker row (inferred,
      verify needed); quarantine `restored`, `closed_by_request`; request `done`. Commit.
4. After commit: `read_lifecycle` must return the new incarnation (else alert, fail closed); registry
   scan; cache eviction (L3); audit; notify the owner or members and the approver.

**Mutual exclusion.** While a request is `applying` or a quarantine is `restoring`/`adopting`: tombstone,
propose, purge, retire and a second request for the label or row refuse with `RESTORE_IN_PROGRESS`. While
`root_state = retiring`: requests, purge of another row on the label and proposals for the label refuse
`RETIRE_IN_PROGRESS`.

### 3.6 Crash recovery (boot, and before any restore, adoption, tombstone, purge or retire on the label) (M6)
- `restoring` and row still disabled: if `agents/<label>` has the recorded `(dev, ino)` rename it back
  (NOREPLACE), `needs_reseal`; remove the users.db side (`remove_personal_mirror` contract at
  `db.rs:1787-1793` amended to name F6 recovery; Shared via `remove_shared_bindings_in_transaction`;
  both bump `acl_generation`); `failed: interrupted`.
- `restoring`, row live, `closed_by_request == request_id`: roll forward. Row live without the match:
  alert, `failed: inconsistent`, nothing else runs for the label.
- `adopting`: see §3.10 step 6. `needs_reseal`: re-verify against the original digest before any request.
- `root_state = retiring`: see §3.12 (crash recovery). `purged` with the tree or label stores still
  present (crash inside purge): finish the purge before anything else runs for the label.
- Every intermediate crash point fails closed: a mirror or binding without a live row reads `Tombstoned`;
  a directory under `agents/` with a disabled row runs nothing.

### 3.7 After restore
- **Old route ids never come back.** Detail routes were revoked and fenced; C4/C3 never reactivate a
  revoked id. Per-employee routes (database, website, channel bots, computer) need
  `isolation.connector_provision` again (new ids, details pinned to the new incarnation).
- **Owner-scoped or tenant routes** usable again are exactly the approved list; old selections are dead
  (A5) and new selections are written at apply for the new incarnation.
- **Work items** (Q9): stay paused; Personal: the owner resumes each; Shared: any Admin resumes each. A
  resume whose approval predates the incarnation needs a fresh approval (A9).

### 3.8 Quarantine cleanup (Q5) and audit (M7)
`isolation.quarantine_purge` (Admin + step-up, R10): any row not `pending`/`restoring`/`adopting`/
`restored`, never with an open request. A row younger than 90 days is an **early purge**: the dialog names
what is lost and the call needs a fresh step-up and `confirm_early = true`; the Personal owner and every
Admin get a notice.
One tx marks the row `purging` and blocks requests; then the tree is removed through held fds (no symlink
following) **together with the label-keyed stores sealed for that row (`quarantine:<hex>-<inc>`) and any
still under `agent:<label>` while no live row holds the label** (R7, N3); then `purged`, `purged_at`. An
adopted row keeps nothing in the tree, so its purge removes only the sealed stores. Purge never touches
the root allocation; when `root_state = allocated` the dialog offers 同時釋放主機資源 (checked by
default), which stages the retire proposal of §3.12 right after the purge commits. The audit record of a
purge keeps the seal digest, manifest totals (entries, bytes) and store marks, so what was destroyed
stays provable after the data is gone. Every transition goes through `audit_and_emit` (actor,
`request_id`, kind, employee hex, incarnation, digest prefix, `self_approved`): tombstone, seal, fence
retry, request, cancel, deny, expire, approve, apply failure, recovery, reseal, adoption steps, purge
(with `early`), retire steps.

### 3.9 Shared tombstone and restore (Q7) [SEC]
- **Tombstone**: `isolation.employee_tombstone` accepts Shared rows (Admin + step-up, as today for
  Personal); the RPC drops `NOT_PERSONAL` for a live Shared row. Quarantine, seal, fences, writer gate,
  stripping and pause are §3.2–§3.3 unchanged; principal snapshot = tenant pin + members (R4). All Admins
  are notified.
- **Request** (`isolation.shared_restore_request`, Admin + step-up): common checks; the request carries
  (R3) `member_selection` ⊆ snapshot members, each still active in the tenant and not offboarded (others
  are shown as dropped; new members go through the normal binding flow after restore), and the route list
  = per-employee nothing (detail routes are re-provisioned) + the tenant routes from
  `active_routes_for(Shared(tenant))` the employee may use, each ticked explicitly. `digest` covers
  request id, employee, incarnation, quarantine digest, tenant, member selection and the ticked route list.
- **Approve**: a different active Admin (R1); self-approval only as the sole active Admin. The approver
  sees the old member list, the selection, the ticked routes, the stripped grants and paused work.
- **Apply**: §3.5 with `restore_shared_bindings`: inserts exactly the selected bindings (`bound_at = now`,
  `is_primary` recomputed, never copied blindly), refuses if any binding or a Personal mirror exists for the
  label, tenant must equal the pin. SharedTenant/SharedHome routes are **re-approved** by the ticked list
  (new selection rows for the new incarnation) and per-employee routes **re-provisioned**; nothing is
  reactivated. Each re-bound member is notified.

### 3.10 Adoption under a new employee id (Q8, F6g) [SEC]
When: the quarantine is `sealed` and `restore_precheck` returns `ROOT_GONE` (R5); v1.81 cases are a
lost or reset spawnd journal and a ReadBack mismatch. `ROOT_UNAVAILABLE` never qualifies.
1. **Request.** Personal: the owner (`me.isolation.request_adoption`, Q2 factor, common + Personal checks,
   `Personal(same owner)` only). Shared: an Admin (`isolation.shared_adoption_request`, step-up,
   `Shared(pinned tenant)` only, member selection and ticked routes as §3.9). `target_label` = the
   original label if free, else chosen by the requester. Free (R6) = no live confirmed row, no marker, no
   open proposal, no `agents/<label>`, no other unpurged quarantine row for it, and spawnd holds no
   entry for it other than Disabled/Retired (known from the `ROOT_GONE` answer; re-checked by spawnd at
   apply). In the ReadBack-mismatch case the old entry still holds the original label, so the requester
   picks a new label; the original label frees after the retire of step 8.
2. **Proposal.** The request stages an Employee proposal (through `prepare_propose`/`stage_prepared`) with
   a digest-covered `adopt_quarantine = <hex>:<incarnation>:<digest>`, the target label, the principal,
   the org entry, and `settings` parsed from the sealed agent.toml after the deterministic transform
   (step 4). Personal: an Admin turns the owner's request into the proposal, as with
   `me.isolation.request_personal_employee` today. Quarantine → `adopting` (bound to the proposal id).
3. **Approve** on the existing proposal approval card (Admin + step-up) with R1: Personal approver ≠ owner,
   Shared approver ≠ requester, unless sole active Admin (`self_approved`). The card shows "adopted from
   <label>, tombstoned <date>", data size, SOUL/CONTRACT summary, the routes and grants that need
   re-provisioning. Deny, expiry or cancel returns the quarantine to `sealed`.
4. **Apply** (the normal Employee apply, after root `Allocate` + `ReadBack` give a new id and UID): verify
   the quarantine digest outside the lock (M1); in the apply tx, NOREPLACE-rename it to
   `agents/<target_label>`, stat re-walk, then the **adoption transform**: drop `.mcp.json`, `.claude/`
   hook settings and any CLI config (regenerated for the new id by `agent_hook_installer` and
   `ensure_mcp_absolute_paths_all`), and on a new label rewrite only `[agent] name` (format-preserving).
   The post-transform `settings` must equal the proposal's. Never touch the old root workspace (the new
   allocation is empty).
5. **Marker (R8).** `employee_dir_markers` gains `origin ∈ {created, adopted}` and `quarantine_ref`. The
   adoption step records `origin = adopted` with a `duduclaw-quarantine-v1` digest of the post-transform
   tree. `check_marker` refuses an `adopted` marker and any non-`ALLOWED_TOP` content in the normal
   proposal flow (`FOREIGN_DIR` unchanged); only the adoption apply step accepts the tree, and only from
   the quarantine named by the proposal. Then `save_confirmed_receipt` (new id, incarnation 0; replaces the
   disabled row on the original label, R6), users.db mirror or selected bindings with an acl bump, quarantine
   `adopted`, `adopted_by_employee`, request `done`. Old employee hex and every authority bound to it stay
   dead (A1–A13); label-keyed stores of the old id are re-keyed under the row, never carried (§3.3, N1).
6. **Failure / crash.** Apply failure after root allocation runs the existing compensation (`Disable` of
   the new Failed item); the tree is renamed back if present, quarantine `needs_reseal`. Boot recovery:
   `adopting` with the proposal not Applied → same rollback; proposal Applied and the new live row's marker
   `quarantine_ref` matches → roll forward to `adopted`; otherwise alert, `failed: inconsistent`.
7. **R7 for every proposal.** Staging or applying any Employee proposal for a label that has an unpurged
   quarantine row refuses `LABEL_QUARANTINED`, unless it is the adoption of that row; the label frees at
   the gateway on purge (which removes its label-keyed stores) or on adoption (which re-keys them under
   the row first), and at spawnd once the old allocation is Retired, Disabled or absent (R7, N3).
8. **Retire the old allocation (R12).** After the adoption is `adopted` and the new employee is live,
   the old allocation is retired through §3.12 (adopt first, then retire). The adoption card offers
   同時釋放舊的主機資源 (checked by default), which stages the retire proposal after the adoption
   commits. When spawnd answered `NotFound` (lost journal), the retire records `root_absent` at once.

### 3.11 RPCs (all dashboard-only, none listed to AI employees, `user_id == "system"` refused)

| Method | Caller | Purpose |
|---|---|---|
| `me.isolation.list_restorable` | owner | own tombstoned Personal employees, state, deadline, `ROOT_GONE` flag |
| `me.isolation.request_restore` / `request_adoption` / `cancel_restore` | owner, Q2 factor | Personal restore / adoption request, cancel |
| `isolation.employee_tombstone` | Admin + step-up | now Personal and Shared |
| `isolation.shared_restore_request` / `shared_adoption_request` | Admin + step-up | Shared requests (§3.9, §3.10) |
| `isolation.employee_restore_decide` / `employee_restore_cancel` | Admin + step-up (decide: R1) | approve / deny a same-id restore; cancel any open request |
| `isolation.employee_propose` (extended) | Admin + step-up | adoption request → proposal |
| `isolation.quarantine_purge` | Admin + step-up (`confirm_early` under 90 days, R10) | F6i purge; optional `retire: true` stages §3.12 after commit |
| `isolation.employee_retire_propose` | Admin + step-up | stage the `EmployeeRetire` proposal for a `purged`/`adopted` row (R11); records `root_absent` on `NotFound`. Approval and apply use the existing proposal card and `apply_next` / `retry` |
| `isolation.employee_list` (extended) | Admin | kind, quarantine state, open request, `self_approved`, `root_state`, label-free date |

### 3.12 Root retire (N2, F6l) [SEC]
Frees a tombstoned employee's root allocation: the spawnd journal entry (and the label it holds), the
loop image with its quota, and the account's run eligibility.

**When and who (R11).** Only a quarantine row in `purged` or `adopted` with `root_state = allocated`; never
while a request on the row or label is open, a row on the label is `restoring`/`adopting`, or the old id
holds a live `confirmed_employees` row (a restored employee must be tombstoned again first). An Admin
calls `isolation.employee_retire_propose` (step-up) or keeps the box ticked in the purge or adoption
dialog; that stages an `EmployeeRetire` proposal, which takes effect when an Admin approves it on the
existing proposal card (Admin + MFA + fresh step-up + CSRF) and `apply_next` runs. R1 does not apply
(owner: Admin + step-up); propose and approve are audited separately and notify every Admin and the
Personal owner.

**Proposal (R12).** New `ConfirmationCategory::EmployeeRetire`, fields digest-covered like any proposal:
`employee_id` (label), `target_proposal_id`, `target_revision`, `target_proposal_digest` (old allocation
binding from `root_item`), `employee_hex`, `uid`, `reason ∈ {purged, adopted}`, `storage = wipe`. Staging
runs the R11 checks in the apply.db tx that sets `root_state = retiring` and `retire_proposal_id`;
approval and apply re-run them (apply.db before users.db, M6).

**Gateway backend** (`EmployeeRetireAuthority` beside `EmployeeAuthority`). `stage` / `publish` /
`read_back` send `Action::Retire` (idempotent; once done every call returns the same Retired receipt). A
Retired receipt matching `root_item` (employee, UID, label, target binding) sets `root_state = retired`,
`retired_at`, and marks the label's `confirmed_employees` row `retired` if it still holds the old id.
`disable_and_read_back` sends `ReadBack` with the old allocation item: Ready means no root effect yet,
so Ok, `root_state` back to `allocated`, and `reject` reaches Rejected. Otherwise Err: retire is
**forward only** (R14), the item stays `Failed` and `retry` resumes the same operation; `reject` past
Pending refuses `RETIRE_FORWARD_ONLY` unless that read-back says Ready, so nothing sticks in `Rejecting`.

**Root operation** (spawnd `Action::Retire`, one call on the lifecycle worker):
1. Validate: category `EmployeeRetire`, status `Applying`. `operation()` takes a kind (`Allocate` 0,
   `Compensate` 1, `Retire` 2; existing bytes unchanged, so old ids still replay).
2. Host gate `NotReady` while any run is live (`host.rs:449`); the gateway retries with backoff for up
   to 24 h, then the card shows 等待主機空閒.
3. Entry by the target operation id, else `NotFound` (`:57` answers `NotFound` for ReadBack and Retire).
   Binding, label, employee, UID must match. `Ready` → go on; `Retiring`/`Retired` under the same retire
   binding → resume or replay; another retire binding, `Planned`, `Disabling`, `Disabled` → `InvalidRequest`.
4. Liveness before any change (`NotReady`, entry stays Ready): no `runs.json` record, no `duduclaw-run-*`
   unit mapped to the employee, no storage lease, `unexpected_processes(uid, None)` empty. A process with
   no run behind it is an intrusion: audit `Intruder`, refuse, leave it to the scanner.
5. Phase `Retiring` with `retire = <binding>`, saved before any side effect.
6. `registry.retire` (account → `Tombstone`; runs resolve Ready accounts only, `host.rs:385`), then
   `recover_locked` (kills anything that appeared after step 4, no-process readback); monitor entry
   cancelled (`host.rs:455-458`).
7. `storage.destroy(employee, operation(retire, Retire), digest)` (refuses a lease, unlinks image and
   mount dir, journaled, resumable); inventory must then show no image and no attachment.
8. `receipt.disabled = true`, phase `Retired`, `retired_at`, save; new root audit `Event::Retired`
   (employee, UID, label, last bytes/inodes). No unit is removed (none may exist at step 4); the locked
   `nologin` passwd/group entry stays, as for every tombstoned account.

**Journal schema 3.** `Phase` adds `Retiring`, `Retired`; `Entry` adds `retire`, `retired_at` (omitted
when None; schemas 1–2 still read, 3 written). Label exclusivity (`:57`, `:110`) treats `Retired` like
`Disabled`; `Allocate` refuses `Retiring`/`Retired`; a retire binding appears on one entry only.

**UID (R15): never returned to the pool.** The existing invariant (`registry.rs:269`, `storage.rs:104`)
is stricter than a quarantine period and removes reuse races with leftover files outright (§7 N4).

**Workspace (R13): wiped.** The loop image (home and work dir during runs) is destroyed, not archived:
the gateway cannot read it, an archive would be a second root store with its own retention, and adoption
never copies it. Owner-visible consequence: files the employee wrote in its isolated workspace and never
sent back are gone. The wipe is an unlink, not an overwrite; host disk encryption covers physical recovery.

**Crash recovery.** spawnd never resumes a retire alone; the gateway drives it. spawnd boot re-quarantines
`Tombstone` accounts (`registry.reconcile`) and keeps a `Destroying` image fenced; a `Retiring` entry
keeps the label. Gateway boot with `root_state = retiring` → `retry`, replaying from step 3; a reply lost
to the 4 s worker timeout resolves the same way. An entry that cannot finish (storage `RecoveryRequired`)
alerts and shows 主機資源釋放未完成; the label stays blocked.

**After retire.** `resolve` / `read_lifecycle` answer `Tombstoned` for a retired row without asking the
root; `restore_precheck` answers `ROOT_RETIRED`. The label is free when every R7 condition holds: no live
row, every quarantine row on it `purged` or `adopted`, no open request or proposal, `root_state ∈
{retired, root_absent}`; a new proposal then replaces the disabled row and spawnd allocates a new entry,
id and UID. **`root_absent`**: when ReadBack of the old item answers `NotFound`, the propose call records
it without a proposal and alerts the operator; remnants of a lost journal are spawnd recovery's job (the
storage adapter already refuses a lost ledger with images, `storage/adapter.rs:43-46`).

## 4. UI (Calm Glass, zh-TW wording; extends the F5 flow)
- **註銷 dialog** (Personal and Shared): estimated size first (「超過 1 GiB 的資料之後無法還原」); afterwards
  whether it can be restored; failed or oversize seal: 「此員工的資料無法完整封存，之後無法還原」 (Q6).
  Shared adds: 「共用員工的成員綁定會一併移除，還原時需逐一重新確認」.
- **Owner → 我的帳號 → 我的個人 AI 員工**: 已註銷 section with 可申請還原（至 YYYY-MM-DD） or the reason.
  申請還原 card: what comes back, owner connections usable again, what needs admin re-provisioning,
  paused work; strongest factor (Q2); then 等待管理員核准 with 取消. On `ROOT_GONE` the button becomes
  以新員工身分接手資料 with a label field (pre-filled with the old label when free) and the note
  「原本的主機資源已不存在，資料會交給一位新的員工，舊的授權與連線都不會帶過去」.
- **Admin → 受保護模式 → AI 員工 → 已註銷**: 註銷時間, 類型 (個人 / 共用), 主人 or 租戶, 隔離區狀態 (可還原 /
  可由新員工接手 / 無法還原：封存失敗或超過大小 / 已超過 90 天保留期限), and for Shared rows 申請還原 and
  以新員工接手 (member checklist, tenant route checklist). Open requests show requester, routes, members,
  核准 / 拒絕 (step-up). The approve button is disabled for the requester with
  「需由另一位管理員核准」; as the only active Admin it reads 自行核准（你是唯一的管理員，會留下紀錄）.
  紀錄 tab lists every §3.8 event with 自行核准 and 提前清除 tags. No new nav page, no `newIn`.
- **清除隔離區** (rows not being restored or adopted; 提前清除 before 90 days, step-up): 「清除後無法還原，
  也不能再由新員工接手。這位員工的記憶、對話紀錄、排程與任務會一併刪除，並通知員工主人與所有管理員。」 The
  checked 同時釋放主機資源 box (also on the adoption card) adds 「主機上這位員工工作區內未回傳的檔案會永久刪除，
  釋放後代號才可給新員工使用。」 Rows show 代號可重新使用：YYYY-MM-DD or 清除並釋放主機資源後即可使用.
- **釋放主機資源** (`purged`/`adopted` rows still holding resources; disabled with 有還原申請進行中 while a
  request is open): same warning, then the proposal card. States: 主機資源保留中 / 釋放中（等待主機空閒） /
  已釋放 / 主機資源釋放未完成（請聯絡維運）.
- Refusals in plain words: 原主人目前無法使用帳號 / 主人的登入方式在註銷後被他人變更 / 已有同名員工或提案 /
  此代號仍保留給已註銷的員工 / 隔離區資料與註銷時不一致 / 主機暫時無法連線，請稍後再試 / 已超過保留期限 /
  連線清單已變更，請重新申請 / 另一個還原正在進行 / 請先設定兩步驟驗證 / 需由另一位管理員核准 /
  主機資源已釋放，無法還原 / 主機資源釋放中 / 主機正在執行其他工作，稍後會自動重試.

## 5. Threat model

| Actor | Attack | Control |
|---|---|---|
| Former owner (offboarded, suspended) | Requests the data back | `owner_eligible` at request, tx1 and the mirror tx; lifecycle re-checks on use |
| Other staff | Requests for someone else | caller == receipt owner; listings filtered; not-owned ids answer `NOT_FOUND` |
| Admin hands Personal data to someone else | Picks another owner, label or id | No owner input; restore keeps label and id; adoption is `Personal(same owner)` only, owner-filed with factor |
| Admin restores or adopts without the owner | Files or applies alone | No admin request RPC for Personal; approval acts only on an owner-filed request bound to a factor proof |
| Admin acts as the owner | Resets password/email/Google/MFA, files, approves | Q3 snapshot refusal (H3); Q2 factor; notice to the tombstone-time address; R1 approver ≠ owner. Residual: a Workspace super-admin who can re-auth at Google is trusted |
| Owner who is an Admin (self-approval) | Approves own restore or adoption | R1: another active Admin must approve; sole-Admin path needs step-up, `self_approved` audit, notices to recent Admins |
| Admin manufacturing "sole Admin" | Demotes or suspends the other Admins, then self-approves | R2 cooling rule (7 days); role changes are audited; notices to every account that held Admin in 30 days |
| Admin abusing Shared restore | Revives a Shared employee with extra members, foreign tenant, or old routes | R1 second Admin; tenant == pin == users.db; members only from the snapshot, each re-checked; tenant routes ticked and digest-bound; detail routes re-provisioned; nothing reactivated |
| Admin abusing adoption | Adopts a tombstoned tree into another person, tenant or a label of their choice to inherit state; triggers adoption while the root is only down | Principal fixed to the quarantine's owner or tenant; R5 `ROOT_GONE` only on a definitive spawnd answer; R7/A13 no label-keyed state carried; MCP/hook/CLI config dropped and regenerated; marker `origin = adopted` single-use per row |
| Any proposer on a reused label (R7) | Applies a new id over a tombstoned label after a journal loss and reads `agent:<label>` memory, sessions, grants | `LABEL_QUARANTINED` until purge or adoption; purge removes and adoption re-keys label-keyed stores under the row; A11 token binds employee id |
| Admin destroying evidence | Purges early and retires to erase a tree, stores or workspace | Purge and retire are separate step-up actions, each audited with seal digest, manifest totals, store marks and workspace usage; root `Event::Retired` lands in spawnd's root-owned log the dashboard cannot edit; notices to the Personal owner and every Admin. Residual: a sole Admin can destroy data; the audit proves what was destroyed, not its content |
| Retire racing restore or adoption, or hitting a live employee | Retire staged while a request is filed or applied, or for a running or restored employee | R11 checks in the staging, approval and apply txs (only `purged`/`adopted` rows, no live row for the old id); `root_state` and requests share apply.db `BEGIN IMMEDIATE`; restore requires `root_state = allocated`; spawnd refuses with any run, unit, lease or UID process (step 4) |
| Leftover files under a reused UID | A new employee gets a UID whose old files or processes survive | UIDs never recycled (R15); `recover_locked` no-process readback; image unlinked before `Retired` |
| Partial wipe | Crash or storage error mid-retire leaves an image, or the label frees too early | `Retiring` saved before side effects; label held until `Retired`; storage destroy journaled with the retire op id and resumable; inventory check before `Retired`; stuck entries alert |
| Journal replay | Replays an old allocation or retire item, or retires a different employee | Retire binds target binding + employee + UID + label; one retire binding per entry; `Allocate` refuses `Retiring`/`Retired`; `ReadBack` refuses non-Ready; distinct operation-id kinds |
| Admin approves a changed scope | Routes or members added after the request | Route and member digests re-checked in tx1 and the apply tx (M5) |
| Admin restores tampered data | Edits the quarantine | Digest in quarantine, stat re-walk after the move, `(dev, ino)`, undo on mismatch. **M3:** apply.db and quarantine share a uid; the seal stops RPC actors only. Optional F6j: spawnd countersign |
| AI employee | Calls restore or revives itself | No MCP tool; dashboard-only RPCs in the not-listed/Admin scope table; tombstoned employees cannot run |
| Holder of an old capability, card or session | Waits for restore or adoption | A1–A13; epoch bumps; fences (H2); new id ⇒ every old authority dead |
| Writer to label-keyed stores, revived bot | Plants state while tombstoned | Writer gate and store marks; grants and `[channels.*]` stripped; bot startup gate (H4, H5) |
| Local same-uid racer | Symlink swap, pre-creating `agents/<label>` | NOREPLACE from held fds; `O_NOFOLLOW` walk; `(dev, ino)`; ctime re-walk |
| Concurrent actions, flooding | Tombstone/propose/purge/retire during a restore; many requests | `RESTORE_IN_PROGRESS`, `BEGIN IMMEDIATE`; one open request per row; L1 caps; lockout counting |

## 6. Task breakdown
- **F6a [SEC] seal and fence at tombstone (Personal and Shared).** `isolation_lifecycle.rs`
  (`tombstone_employee`, row keyed by incarnation, `root_item`, principal snapshot incl. Shared members,
  pause work), new `isolation_quarantine.rs` (manifest, seal), `isolation_connector_lifecycle.rs` (revoke
  employee detail routes, pending fences), approval invalidation, cache eviction, `db.rs`
  `remove_shared_bindings_in_transaction`, audit. Tests (`tombstone_`): `seals_manifest_outside_lock`,
  `oversize_still_tombstones_and_reports`, `absent_dir_seals_absent`,
  `records_pending_fence_on_unavailable`, `revokes_employee_detail_routes_only`,
  `keeps_tenant_routes_for_other_employees`, `snapshots_owner_identity`,
  `shared_snapshots_tenant_and_members`, `shared_removes_bindings_and_bumps`,
  `legacy_unpinned_shared_refused`, `strips_channel_sections_and_grants`, `audited`;
  `manifest_rejects_hardlink_foreign_uid_xattr`, `manifest_orders_raw_bytes`,
  `second_tombstone_after_restore_uses_new_key`.
- **F6b [SEC] incarnation and authority inventory.** Schema, lifecycle, `authority.rs`, background,
  delegation, route selections, C6 details, sessions, agent token. Tests: one per A1–A13 row plus
  `apply_preserves_incarnation`, `legacy_row_reads_incarnation_zero`, `new_id_starts_at_incarnation_zero`.
- **F6c [SEC] same-id restore engine.** New `isolation_restore.rs` (journal, common checks, R1 rule,
  `restore_precheck` with R5 classes, apply, recovery); `db.rs` `restore_personal_mirror`,
  `restore_shared_bindings`, amended `remove_personal_mirror` contract, `credential_changes` if missing.
  Tests (`restore_`): `same_id_same_owner_ok`, `refuses_ineligible_owner` (3 points),
  `refuses_credential_change_by_non_owner`, `allows_owner_own_credential_change`,
  `refuses_incomplete_tombstone`, `refuses_pending_fence_and_retries`, `refuses_after_90_days`,
  `refuses_unsealed_or_oversize`, `rechecks_quota`, `refuses_open_proposal_or_marker`,
  `integrity_mismatch_in_quarantine`, `ctime_change_after_move_moves_back`, `routes_changed_refused`,
  `store_marks_changed_refused`, `org_entry_changed_refused`, `root_live_process_refused`,
  `transport_error_is_unavailable_not_gone`, `readback_mismatch_is_gone`, `crash_*` (rename, users.db,
  commit), `live_row_without_request_match_alerts`, `every_transition_audited`,
  `apply_path_still_never_revives_disabled_row`.
- **F6c2 [SEC] approval rule (R1, R2).** Shared by restore decide and the adoption card. Tests:
  `owner_admin_cannot_self_approve_with_other_admin`, `shared_requester_cannot_self_approve`,
  `sole_admin_self_approves_with_step_up_and_audit`, `inactive_or_no_mfa_admin_not_counted`,
  `self_approval_refused_within_cooling_window`, `admin_count_read_inside_apply_tx`.
- **F6d [SEC] RPCs** (§3.11) in `handlers/isolation_employee_rpc.rs` (METHODS `:63-64`) + child module
  `isolation_employee_restore.rs`. Tests: `request_only_by_owner`, `request_requires_strongest_factor`,
  `factor_failures_count_toward_lockout`, `decide_requires_admin_step_up`, `admin_cannot_file_personal_request`,
  `request_rate_limits`, `notice_caps`, `request_expires`, `cancel_by_requester_or_admin`,
  `restore_rpcs_not_listed_to_ai`, `tombstone_accepts_shared`.
- **F6e UI.** Protected-mode page, 我的帳號 tab, `web/src/lib/api.ts`, i18n ×3; design canvas first
  (`feedback_logic_first_then_design`): tombstone dialog, owner card incl. adoption, admin Shared request
  with member/route checklists, approve button states, early purge and retire dialogs, root row states.
- **F6f.** G2 `iso_personal_chain.rs`: `iso_root_e2e_tombstone_restore_same_uid`,
  `iso_root_e2e_shared_tombstone_restore`, `iso_root_e2e_adopt_after_journal_reset` (new UID, old
  capability and token refused, root workspace empty), `iso_root_e2e_purge_retire_frees_label` (new
  proposal on the label gets a new id and UID), `iso_root_e2e_adopt_then_retire_old`; CHANGELOG Added;
  F5 decision 4 done;
  `docs/features` isolation page.
- **F6g [SEC] adoption (Q8, scheduled).** §3.10: proposal field `adopt_quarantine`, adoption transform,
  marker `origin`/`quarantine_ref` and `check_marker` refusal, compensation and recovery, Personal and
  Shared requests. Tests (`adopt_`): `requires_root_gone`, `original_label_when_free`,
  `new_label_rewrites_agent_name_only`, `drops_mcp_hook_and_cli_config`, `never_copies_root_workspace`,
  `old_id_authorities_dead`, `label_stores_sealed_not_carried`, `personal_same_owner_only`,
  `shared_same_tenant_only`, `normal_proposal_refuses_adopted_marker`, `digest_mismatch_refused`,
  `failure_compensates_new_allocation`, `crash_rolls_back_or_forward`, `single_use_per_row`.
- **F6h [SEC] store and channel gates (H4, H5, R7).** Writer gate for memory, sessions, tasks, cron;
  bot startup gate; `LABEL_QUARANTINED` in `prepare_propose`/`stage_prepared` and apply; label-store
  re-key and purge. Tests: one per writer path, `channel_bot_needs_live_lifecycle_and_route`,
  `proposal_on_quarantined_label_refused`, `purge_removes_label_stores`.
- **F6i purge (Q5, N3, R10).** Tests (`purge_`): `early_needs_confirm_and_step_up`,
  `early_notifies_owner_and_admins`, `refused_with_open_request`, `no_symlink_follow`,
  `removes_tree_and_label_stores`, `adopted_row_removes_sealed_stores_only`, `crash_finishes_first`,
  `audit_keeps_digest_and_totals`. **F6j (optional, M3)**: spawnd countersign of the seal digest.
- **F6k [SEC] Shared restore (Q7).** §3.9 requests, member selection, tenant route ticks. Tests
  (`shared_restore_`): `tenant_must_equal_pin`, `members_only_from_snapshot`, `inactive_member_dropped`,
  `no_auto_rebind`, `tenant_routes_reapproved_not_reactivated`, `detail_routes_need_provisioning`,
  `second_admin_required`.
- **F6l [SEC] root retire (N2, §3.12).** `duduclaw-isolation`: `ConfirmationCategory::EmployeeRetire`,
  `ErrorCode` reuse. `duduclaw-migration/src/allocation.rs`: `Action::Retire`, retire field parsing and
  validation, `operation()` kinds, Response schema 2 `error`; `confirmation.rs`: forward-only `reject`.
  `duduclaw-spawnd`: journal schema 3 (`Retiring`/`Retired`, `retire`, `retired_at`), retire procedure,
  `NotFound` at `:57`, liveness checks, `Event::Retired`. Gateway: `EmployeeRetireAuthority`,
  `employee_retire_propose`, purge/adoption hooks, `root_state` and `root_item`, lifecycle `ROOT_RETIRED`.
  Unit tests: `allocation_retire_requires_applying_retire_item`, `allocation_retire_fields_digest_bound`,
  `allocation_operation_kinds_distinct_and_stable`, `allocation_response_schema2_error_code`,
  `journal_schema3_reads_schema1_and_2`. Root tests (`iso_root_retire_`): `refuses_live_run_unit_lease_or_process`,
  `refuses_target_mismatch`, `refuses_non_ready_entry`, `replay_same_op_idempotent`,
  `second_retire_binding_refused`, `crash_after_retiring_resumes`, `partial_destroy_resumes`,
  `frees_label_for_new_allocation`, `uid_never_recycled`, `allocate_and_readback_refused_after`,
  `audits_retired`. Gateway tests (`retire_`): `only_purged_or_adopted`, `refused_with_open_request`,
  `refused_for_live_old_id`, `reject_forward_only_after_retiring`, `compensation_ok_while_ready`,
  `retired_row_reads_tombstoned_without_root`, `not_found_records_root_absent`,
  `label_free_only_after_all_r7_conditions`, `adoption_then_retire_old_allocation`, `audited_and_notified`.

## 7. Items that need the owner
Resolved in r4: R2 (7 days), N1 (nothing label-keyed carried), N3 (90-day hold, early purge), N2 (retire
in v1.81). Open:
- **N4 UID after retire.** The r4 brief asked to return the UID to the pool after a quarantine period. The
  code never recycles a UID (`registry.rs:269`, `storage.rs:104`), which already rules out reuse races.
  (A) keep never-recycle; (B) recycle after 30 days behind the existing `no_existing_file_owner` scan.
  Recommendation: A. 30 000 UIDs and 4 096 journal entries are a lifetime budget per host either way
  (the journal limit binds first); pruning `Retired` entries is a later item if a host ever nears it.

**Owner decision 2026-11-08 (N4):** UIDs are never recycled after retire (option A). Retire frees storage, units and the label; the UID number stays burned. The 4096-entry allocation journal remains the per-host lifetime cap (track as a follow-up: journal compaction for retired entries).
