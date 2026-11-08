# F6: restore a tombstoned protected employee (v1.81.0, design for security review)

Status: design draft, 2026-11-08, not implemented. Code read at `feat/iso181-w12-20261108` @ 5b067e31d
(paths below are under `crates/`). Owner decision (F5 doc, decision 4): the admin UI offers restore of a
tombstoned employee, back to the same employee id, only to the original owner, who must still be
employed and must consent, and only while no new employee holds the name. [SEC] = Daybreak review
before merge. Inferences are marked "(inferred, verify)".

## 1. What exists today

| Fact | Where |
|---|---|
| Tombstone is Personal-only: Shared employees get `Denied` and the RPC answers `NOT_PERSONAL` | `duduclaw-gateway/src/isolation_lifecycle.rs:285`, `handlers/isolation_employee_rpc.rs:815-819` |
| In one apply.db `BEGIN IMMEDIATE` tx: `rename(agents/<label>, isolation-quarantine/<id hex>-<label>)`, chmod 0700, set `receipt.disabled = true` on the `confirmed_employees` row, delete the `employee_dir_markers` row. After commit the users.db mirror and all bindings are removed | `isolation_lifecycle.rs:273-304`, `:321-361` |
| The target check and `rename` are separate steps (TOCTOU; `rename` silently replaces an empty directory that appears at the target) | `isolation_lifecycle.rs:343-348` |
| No digest of the quarantined tree is taken and no audit row is written (inferred, verify; nothing in `tombstone()` calls `audit_and_emit`) | `handlers/isolation_employee_rpc.rs:810-821` |
| The root side is untouched. The spawnd entry stays `Phase::Ready`, so `ReadBack` still returns the same employee id, UID, quota and principal. `Action::Disable` is accepted only for Failed/Rejecting/Rejected proposals, so an Applied employee cannot be disabled at the root | `duduclaw-spawnd/src/allocation.rs:80-98`, `duduclaw-migration/src/allocation.rs:94-103` |
| spawnd refuses any new allocation for a label that has a non-Disabled entry. While the tombstoned entry is Ready, no other employee can be allocated under that label | `duduclaw-spawnd/src/allocation.rs:57` |
| The lifecycle refuses `prior.disabled` (apply.db copy) before the root ReadBack. `recheck_with` compares the whole `EmployeeLifecycle` struct | `isolation_lifecycle.rs:153`, `:137-145`, `:76-87` |
| The apply path never revives a disabled row of the same employee (`checked_tenant` returns `Denied`). This invariant must stay | `handlers/migration_authority_employee.rs:150` |
| A new proposal may be staged for a tombstoned label (`check_marker` raises `LABEL_TAKEN` only for live rows). Its apply would then fail at the root (label still Ready) | `handlers/isolation_employee_dirs.rs:112-134` |
| Connectors: tombstone changes nothing in the catalog. `Personal(owner)` routes belong to the person and may serve several of that owner's employees (C4). C6 detail rows (database/computer/channel) are pinned to the employee id + agent | `isolation_connector_details.rs:24`, `:422-444` |
| Background: Personal targets never get receipts (B3, `PERSONAL_REFUSAL`). Personal delegation approval cards are bound to `(owner, source agent, source employee, target, task digest)`, 24 h TTL, single-use | `isolation_background/personal_delegation.rs:102-111`, `:120-138` |
| Home-wide stores keyed by label (sessions, memory namespace = bare id, tasks, cron) are not moved by tombstone (inferred, verify) | CLAUDE.md "Memory"; `~/.duduclaw` layout |

**The main gap.** Tombstone keeps the employee id, the UID and the root receipt, and `EmployeeLifecycle`
has no field that changes when the row comes back. If restore only flipped `disabled` back to false,
every capability, `PreparedAuthority` pin, connector selection, C6 detail pin and approved but
unconsumed delegation card issued before the tombstone would compare equal again and come back to
life. Restore therefore needs an **incarnation** counter (§3.1). It also has to revoke the
employee-pinned connector routes at tombstone time (§3.4).

## 2. Decisions proposed

- **Same label and same employee id only.** The label is digest-covered in the Applied proposal and
  copied into the immutable root receipt (`Receipt.label`). Home-wide stores also key on it. Renaming
  would need a new proposal, which means a new employee id. Precondition: no live employee and no open
  Employee proposal holds the label. Today the first part holds by construction (spawnd `:57`).
- **Same-id restore is possible today** because nothing disables the root entry: `ReadBack` on the
  Applied proposal still returns `Ready`. If a root retire action is added later (A2 open item 1), it
  calls `storage.destroy`, and same-id restore of a retired employee becomes impossible. For that case
  see §6 F6g: a new Employee proposal adopts the gateway-side quarantine under a new id, with owner
  consent. The root workspace contents cannot be carried over.
- **Personal only** in v1.81, because Shared employees cannot be tombstoned yet (open question 7).
- **Initiation and consent are two parties.** An Admin with step-up files a restore request. The owner
  consents in 我的帳號 → 我的個人 AI 員工, and the restore runs on that consent (open questions 1 and 10).
  No request field names an owner. The owner is always the immutable receipt principal.

## 3. Backend design

### 3.1 Incarnation [SEC]
- apply.db `confirmed_employees` gets a column `incarnation INTEGER NOT NULL DEFAULT 0`, added in
  `ensure_employee_schema` (`migration_authority_employee.rs:122-133`, the same pattern as `tenant_id`).
  It is not added to the root `Receipt`, which is `deny_unknown_fields` and root-owned.
- `EmployeeLifecycle` gets `incarnation: u32`, read in `resolve` from the same row read. `recheck_with`
  already compares the whole struct, so every pin taken before a restore reads `Stale` and maps to
  `Revoked`.
- Only restore writes the column, and it only increments. `save_confirmed_receipt` preserves the stored
  value on `ON CONFLICT` and writes 0 for a new employee id.
- The delegation `Gate` payload gets `source_incarnation`, so a card filed before the tombstone never
  `binds` again (`personal_delegation.rs:102-122`).

### 3.2 Sealing at tombstone (changes to `tombstone_personal_employee`) [SEC]
1. Use `renameat2(RENAME_NOREPLACE)` instead of the check-then-`rename`.
2. Inside the same apply.db tx, after the move, build a **manifest** of the quarantined tree: a walk
   that never follows symlinks; each entry contributes (relative path, type, mode, size, SHA-256 for
   regular files, `readlink` text for symlinks); sockets, FIFOs and device files make the manifest
   invalid. Bounds: ≤ 65 536 entries, ≤ 1 GiB of file bytes, depth ≤ 16. Write a new apply.db row
   `employee_quarantine(employee_hex PK, label, owner_user_id, quarantined_at, dev, ino, digest, seal)`
   with `seal ∈ {sealed, oversize, invalid}`. A walk over the bounds never blocks the tombstone
   (handover must still work), but restore then refuses (open question 6). The digest reuses the
   length-prefixed `put` framing from `isolation_employee_dirs.rs:31-36` with a new domain tag
   `duduclaw-quarantine-v1`.
3. After the commit, and before the mirror removal:
   - revoke every catalog route whose C6 detail pin names this employee, through
     `isolation_connector_lifecycle::revoke_with` (`:157-166`; it deletes the details, cancels relink
     hints and fences before dropping material);
   - invalidate the Pending/Approved `isolation_personal_delegation` cards whose `source_employee`
     equals this employee (`ApprovalStatus::Invalidated`, `invalidated_reason`; `approval/mod.rs:260`,
     `:357`; the exact broker call needs checking, inferred, verify).
   Failures are logged and audited but do not undo the tombstone, which stays fail-closed: the
   lifecycle is already `Tombstoned`, and the incarnation covers anything missed.
4. Audit `isolation.employee_tombstoned` through `security_autopilot::audit_and_emit` (the pattern of
   `isolation_connector_lifecycle.rs:208-212`): actor, label, employee hex, seal state, routes revoked,
   cards invalidated.

Lock hold time: hashing ≤ 1 GiB inside the apply.db writer lock can exceed other writers' 5 s busy
timeout. Those writers fail as `Unavailable`, which is acceptable for a rare admin action. Document it
in the UI ("可能需要數十秒").

### 3.3 Restore journal and state machine [SEC]
apply.db table `employee_restores(request_id PK, employee_hex, label, owner_user_id, incarnation,
digest, admin_user_id, created_at, expires_at, state, decided_at, failure)`. A partial unique index
allows one open request per employee (`state IN ('requested','applying')`). States:
`requested → applying → restored | failed`, and `requested → cancelled | denied | expired`.
TTL is 72 h (open question 5 covers retention).

**Request** (`isolation.employee_restore_request`, Admin + step-up via `authorize_step_up`). In one apply.db
tx: the row is disabled with this employee id; the principal is `Personal`; a quarantine row is `sealed`;
no `employee_dir_markers` row and no open Employee proposal for the label (`label_pending`,
`isolation_employee_rpc.rs:711`); `agents/<label>` is absent; the owner is eligible (`owner_eligible`
rule); a root precheck passes (below). The request records `incarnation` and `digest`. Rate limits: at
most 5 requests per admin per 24 h and one open request per employee.

**Root precheck** (new `lifecycle::restore_precheck`): the `resolve` steps without the `prior.disabled`
gate. The Applied proposal is found, `spec.binding == prior.principal_binding`, and `ReadBack` returns a
receipt that is not disabled, with the same employee, UID, label, quota and principal. Any failure
(including a future root retire) is `ROOT_UNAVAILABLE`.

**Consent and apply** (`me.isolation.decide_restore`, caller == `owner_user_id`, password re-entry; open
question 2):
1. tx1 (short, `BEGIN IMMEDIATE`): re-run every request-time check, compare `incarnation`/`digest`
   with the request, check the owner (fresh users.db read), then set `state = applying`. Commit.
2. tx2 (`BEGIN IMMEDIATE`, `synchronous=FULL`, held to the end):
   a. `renameat2(quarantine → agents/<label>, RENAME_NOREPLACE)`. The (dev, ino) must match the
      quarantine row.
   b. Recompute the manifest **in place** and compare it with the sealed digest. This validates the
      artifact that takes effect. On a mismatch, rename back, set `failed: integrity`, audit and stop.
   c. `record_personal_mirror(home, label, owner)`. It re-checks owner eligibility, inserts the mirror
      and binds the owner in one users.db tx (`isolation_lifecycle.rs:227-235`). This follows the
      `save_confirmed_receipt` order: the mirror commits while the apply.db lock is held, before the row
      goes live.
   d. `UPDATE confirmed_employees SET receipt = <disabled=false>, incarnation = incarnation + 1 WHERE
      label = ? AND receipt = <old> AND incarnation = <old>` (exactly one row). The quarantine row
      becomes `restored`, the request becomes `restored`. Commit.
3. After commit: `read_lifecycle` must return Ok with the new incarnation (if not, alert; the state stays
   fail-closed), then `registry.scan()`, audit `isolation.employee_restored`, and a `notify_push` notice
   to the owner and the requesting admin.

**Mutual exclusion.** While a request is `applying`, tombstone, propose, purge and a second restore
for the label refuse with `RESTORE_IN_PROGRESS`. This matters: a tombstone replay on a disabled row would
otherwise remove the mirror that step 2c just wrote (`isolation_lifecycle.rs:286-303`).

**Crash recovery** (gateway boot, and before any restore or tombstone on the label):
- request `applying` and row still disabled: roll back. If `agents/<label>` has the recorded (dev, ino),
  rename it back with NOREPLACE. Remove the mirror (`remove_personal_mirror`, ignoring `Stale`). Set
  `failed: interrupted`. The owner must consent again on a new request.
- request `applying` and row live with incarnation = request + 1: roll forward to `restored`.
- Every intermediate crash point fails closed. A mirror without a live row reads `Tombstoned`, because
  `disabled` is checked first. A directory under `agents/` with a disabled row runs nothing.

### 3.4 Connectors, receipts and runs after restore
- **Old route ids never come back.** Detail-bearing routes were revoked at tombstone (§3.2), and C4/C3
  never reactivate a revoked id. An admin provisions new routes through `isolation.connector_provision`,
  which creates new ids and new details pinned to the same employee id.
- **Owner-scoped routes** (`Personal(owner)` google/notion/github, and SharedHome fallback) were never
  revoked, because other employees of the same owner may use them. The consent card lists them by
  provider and account label. Consenting is the owner's explicit re-approval (open question 4). Old
  selections are dead through the incarnation.
- **Receipts and capabilities**: B3 receipts never existed for a Personal employee. Delegation cards are
  invalidated at tombstone and incarnation-bound (§3.1). Every live capability or `PreparedAuthority`
  from before the tombstone reads `Stale` → `Revoked`. Nothing is re-approved automatically.
- **Work items**: tasks, goals and cron rows keyed by the label survive. After restore they stay paused
  (`needs_human`) until the owner resumes them (inferred, verify how the goal loop records
  `Tombstoned` failures; open question 9).

## 4. UI (Calm Glass, zh-TW wording; extends the F5 flow)
- **Admin → 受保護模式 → AI 員工**: an 已註銷 row shows 註銷時間, 主人, a 隔離區狀態 badge (完整可還原 /
  無法驗證完整性 / 主機資源已釋放) and a 還原 action. The dialog explains: 「只能還原給原主人，主人需在
  我的帳號同意後才會生效；舊的連接器帳號不會恢復，需要重新設定」. Then step-up and
  `isolation.employee_restore_request`. The row then shows 等待主人同意 (expiry time) with 取消
  (`isolation.employee_restore_cancel`, step-up). The 紀錄 tab adds tombstone and restore events.
- **Owner → 我的帳號 → 我的個人 AI 員工**: a 還原請求 card shows the employee name, the requesting admin,
  the tombstone date and the owner-scoped connections that will be usable again. Buttons 同意還原 /
  拒絕. 同意 asks for the password again. Lists come from `me.isolation.list_restore_requests` (own
  requests only).
- Refusals in plain words, without internal terms: 原主人目前無法使用帳號 / 已有同名員工或提案 /
  隔離區資料與註銷時不一致 / 主機資源已釋放，無法還原到同一位員工 / 另一個還原正在進行.
- Not a new page, so no `newIn` badge. The card and the action carry 1.81.0 per
  `feedback_new_feature_badge` if the owner wants one.

## 5. Threat model

| Actor | Attack | Control |
|---|---|---|
| Former owner (offboarded, suspended, forced password change) | Consents to get the data back | `owner_eligible` at request, at consent (tx1) and inside the mirror tx (2c). The lifecycle re-checks on every use afterwards |
| Other staff | Consents for someone else, or sees the card | `decide_restore` requires caller == receipt owner; listings are filtered to the caller; not-owned requests answer `NOT_FOUND` |
| Admin hands the data to someone else | Picks a different owner or label | No owner or label input exists. The owner comes from the immutable root receipt principal and the label from the digest-covered proposal. Restore cannot create a new employee id |
| Admin restores without consent | Calls the apply step directly | Apply runs only inside the owner's `me.*` call. Admin RPCs can only request or cancel |
| Admin impersonates the owner | Resets the owner's password, logs in, consents | Forced password change blocks consent. Open question 3 adds: refuse when an admin reset the owner's credentials after the tombstone (inferred: needs a users.db reset-actor field). The owner is notified at request and at completion |
| Admin restores tampered data | Edits the quarantine while it is held | Sealed digest is verified in place after the move; (dev, ino) must match; on a mismatch the move is undone |
| AI employee (the tombstoned one, or another) | Calls restore, or revives itself | No MCP tool; dashboard-only `isolation.*`/`me.*` RPCs; `user_id == "system"` refused (`isolation_employee_rpc.rs:182`); a tombstoned employee cannot run. Add the methods to the not-listed/Admin scope table |
| Holder of an old capability, selection or card | Waits for the restore to reuse it | Incarnation (§3.1); detail routes revoked; cards invalidated |
| Local same-uid racer | Symlink or directory swap, pre-creating `agents/<label>`, swapping stores | `renameat2(NOREPLACE)`; no symlink following; quarantine root 0700 with owner check; (dev, ino) check; in-place digest; stores only through `fs_safe::sqlite::open_existing`. A same-uid process can still edit `agents/<label>` after restore, which is out of scope (same as today) |
| Concurrent admin actions | Tombstone, propose or purge during a restore | `RESTORE_IN_PROGRESS` gate plus `BEGIN IMMEDIATE`; one open request per employee |

## 6. Task breakdown

- **F6a [SEC] seal and fence at tombstone.** Files: `isolation_lifecycle.rs` (NOREPLACE, quarantine row,
  audit), new `isolation_quarantine.rs` (manifest), `isolation_connector_lifecycle.rs`
  (`revoke_employee_detail_routes`), `isolation_background/personal_delegation.rs` +
  `approval/mod.rs` (invalidate by source employee). Tests: `tombstone_seals_manifest`,
  `tombstone_oversize_tree_still_tombstones_unsealed`, `tombstone_noreplace_refuses_existing_target`,
  `tombstone_revokes_employee_detail_routes_only`, `tombstone_invalidates_source_delegation_cards`.
- **F6b [SEC] incarnation.** Files: `migration_authority_employee.rs` (schema, preserve on conflict),
  `isolation_lifecycle.rs` (field, read), `personal_delegation.rs` (payload). Tests:
  `lifecycle_incarnation_change_is_stale`, `apply_preserves_incarnation`, `legacy_row_reads_incarnation_zero`,
  `delegation_card_from_prior_incarnation_never_binds`.
- **F6c [SEC] restore engine.** New `isolation_restore.rs` (journal, `restore_precheck`, apply, recovery
  hook at gateway boot). Tests: `restore_same_id_same_owner_ok`, `restore_refuses_ineligible_owner` (3
  points), `restore_refuses_open_proposal_or_marker`, `restore_refuses_existing_agents_dir`,
  `restore_integrity_mismatch_moves_back`, `restore_root_readback_mismatch_refused`,
  `restore_crash_after_rename_rolls_back`, `restore_crash_after_mirror_rolls_back`,
  `restore_crash_after_commit_rolls_forward`, `tombstone_refused_while_restore_applying`,
  `old_capability_revoked_after_restore`, `apply_path_still_never_revives_disabled_row`.
- **F6d [SEC] RPCs.** `handlers/isolation_employee_rpc.rs` (METHODS `:61-62`; new `isolation_employee_restore.rs`
  child module): `isolation.employee_restore_request|restore_cancel` (Admin + step-up),
  `me.isolation.list_restore_requests|decide_restore` (owner, password re-entry); `employee_list` adds
  the quarantine and restore state; `prepare_propose`/`stage_prepared` refuse during an open restore.
  Tests: `restore_request_requires_admin_step_up`, `restore_consent_only_owner`,
  `restore_request_rate_limited`, `restore_request_expires`, `restore_cancel_by_admin`.
- **F6e UI.** `web/src/pages/` protected-mode page, the 我的帳號 tab, `web/src/lib/api.ts`, i18n ×3.
  The design canvas comes first (`feedback_logic_first_then_design`).
- **F6f.** G2 `iso_personal_chain.rs` adds `iso_root_e2e_tombstone_restore_same_uid` (root ReadBack
  unchanged, old capability refused, new one works); CHANGELOG Added; F5 doc decision 4 marked done;
  `docs/features` isolation page.
- **F6g (deferred).** Adopt under a new id once a root retire action exists: an Employee proposal for
  `Personal(same owner)` with a digest-covered `adopt_quarantine = <employee hex>:<digest>`, owner
  consent, single-use (the quarantine row becomes `adopted`).

## 7. Open questions for the owner (recommendation first)

1. Who starts it? **A (recommended): the admin requests and the owner consents.** B: the owner asks and
   the admin approves. C: either side can start.
2. Consent authentication: **password re-entry (recommended)**, or MFA when enrolled, or the current
   session only.
3. Admin credential reset: **refuse consent when an admin reset the owner's password or MFA after the
   tombstone (recommended; needs a reset-actor record)**, or notify only.
4. Owner-scoped connections (Google, Notion, GitHub): **listed on the consent card and approved by
   consenting (recommended)**; or advance their generation; or require new provisioning for all.
5. Retention: **restore allowed for 90 days, after which the quarantine may be cleaned up (recommended;
   no cleanup tool exists yet)**, or no limit.
6. Quarantine that is unsealed or too large: **refuse restore (recommended)**, or allow it with a
   warning both sides accept.
7. Shared employees cannot be tombstoned today. **Defer restore for Shared (recommended).** If added
   later: Admin-only, tenant must equal the apply.db pin and the users.db tenant.
8. After a future root retire: **adopt under a new id (F6g), deferred (recommended)**, or no restore at all.
9. Tasks and goals after restore: **paused until the owner resumes them (recommended)**, or resume
   automatically.
10. When it takes effect: **at the owner's consent (recommended)**, or after a final admin confirmation
    following consent.
