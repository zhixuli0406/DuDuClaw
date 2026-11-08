# Org chart editing (reporting lines and departments)

Who an AI employee reports to (`reports_to`) and which department it belongs to decide who may delegate to whom. The authoritative record is `<home>/org.toml`, which no employee can write. Each employee's `agent.toml` holds a display copy. RM-027 adds the gateway API for changing both from the dashboard. **The org chart page is still read-only; the editing UI ships in a later release.**

## Who may change what

| Role | What happens |
|------|--------------|
| Admin | The change takes effect **immediately**. The dashboard first shows a before/after confirmation, then sends the `base` it showed. If the org chart changed in the meantime, the change is refused and nothing is written. Every applied change is audited as `org_change_applied` with the before/after values and who made the change. |
| Manager | The change becomes a **proposal**. It takes effect only after a current Admin approves it in the approval centre. Managers may propose a change for an employee they could assign work to (an Operator or Owner binding, or an Operator-level team grant). The new supervisor must be someone they can see. Moving an employee to the top level (no supervisor) is left to an Admin. |
| Employee | Cannot change the org chart. |

The role is read again from the user database on every call, so a demotion takes effect on the next request. An employee can still never change its own placement (`org_field_guard`).

## Rules every change must pass

The same check (`duduclaw_core::org_change::validate_org_change`) runs on `agents.update`, on every `org.*` call and again when an Admin approves a proposal:

- The employee and the new supervisor must exist.
- An employee cannot report to itself.
- The change must not create a loop. The chain is followed through `org.toml`, not the display copy, so a loop that exists only in the authority is still caught.
- The department name must be valid. Once any department exists, the new one must already exist: it is used by an employee or has a shared-wiki/shared-skill folder (create it under **Departments** first). Clearing the department is always allowed.

`agents.update` now refuses an invalid `reports_to` with the same error. It was previously written unchecked. Its department behaviour is unchanged.

The dashboard raw editor (`config.raw.set`, Admin) follows the same path when an `agent.toml` edit changes `[agent] reports_to` or `department`: it takes the global org lock, runs the whole-tree check (existence, self, cycle) against the authority, writes the authority first and the file second, and puts the authority back if the file write fails. If the check fails, the raw save is refused with the validation error and nothing is written. An edit that leaves both fields as they were never touches the authority.

### Protected from AI employees

Besides `org.toml`, an AI employee cannot write, hold or delete `<home>/locks/org-mutation.lock` or anything under `<home>/org_change_journal/` (file-write guard and shell guard). Any shell command that names the lock file is refused, even a read-only one such as `flock … sleep`, because holding the lock would stall every org change.

### How a change is written

- **One lock for the whole tree.** Every org writer takes the same cross-process lock, `<home>/locks/org-mutation.lock`. That covers the `org.*` calls, approving a proposal, the org fields of `agents.update`, the MCP `agent_update` org path and `duduclaw org sync`. Inside the lock the writer reads the authority again, compares the `base`, checks the rules against the whole tree and then commits. Two opposite moves at the same time (A under B, B under A) cannot both get through.
- **The authority must be usable.** If `org.toml` was set up once but is now missing, unreadable, not valid TOML or written by a newer DuDuClaw, every org change is refused with `org_authority_unavailable`. The display copies are never used in its place, and the file is never rebuilt from an empty store. Read-only views (`org.placement`, `org.pending_changes`, the org chart) keep working, and `org.placement` reports the problem in `authority_error`. Saving other employee fields keeps working too. Only `duduclaw org sync` can rebuild a lost or corrupt file, and only as a full rebuild. The corrupt original is kept as `org.toml.corrupt`. A newer-schema file is never rewritten.
- **Authority first, then the display copy.** If the display copy cannot be updated, the change still stands: the authority is the truth. The gateway records `org_change_mirror_drift` and answers `mirror_sync_failed`. To retry, apply the same placement again. The gateway then answers `status:"mirror_synced"`. Do **not** run `duduclaw org sync` in this state, because it would copy the old display values back into the authority. If `agents.update` fails to write `agent.toml`, it puts the previous authority entry back.

## Proposals and approvals

- Only one proposal can wait for each employee at a time. A second one is refused with `pending_exists` and the waiting request's id.
- The request is decided in the dashboard only. Messaging apps get a plain notice without buttons, and a decision sent from a channel is refused.
- On approval the gateway checks the rules again and confirms the employee's placement is still the one the proposal was made against. Only then does it write the authority and the display copy, and it records the decision only after the write succeeded. If the placement changed while the request waited, or the change is no longer valid, the request is **denied** and nothing is written.
- Recording the decision can fail after the change was applied. In that case the request stays waiting (`decision_unrecorded`) and no final audit row is written. The applied result is saved under `<home>/org_change_journal/` with the approval id. Approving again completes the original approval and does not apply the change a second time. A denial of an already applied request is refused (`already_applied`). Audit rows and the channel notice update are written only after the decision is recorded.
- Rejection writes nothing. A request expires after 3 days, and expiry counts as a denial.
- If an Admin applies a direct change to an employee who has a waiting proposal, the proposal is withdrawn. A proposal whose approval was already applied is left for its retry.
- Each proposal records its proposer as `on_behalf_of`. With `[approval] forbid_self_approval = true`, nobody can approve their own proposal, Admins included. The proposer in the request is checked as well. Rejecting your own proposal is still allowed.

Audit events: `org_change_requested`, `org_change_applied` (with `actor`, plus `approved_by` and `proposer` when it came from a proposal), `org_change_rejected` (`why`: `rejected`, `expired`, `stale`, `invalid`, `malformed`), `org_change_mirror_drift`, `org_change_mirror_resynced`, `org_change_decision_unrecorded`, `org_change_decision_mismatch`.

## Gateway RPCs

All four are available in both editions; the org chart is cross-employee, not cross-person.

| Method | Role | Params | Result |
|--------|------|--------|--------|
| `org.placement` | Manager+ | `{agent_id}` | `{agent_id, placement:{reports_to, department}, source:"authority"\|"mirror", mirror_drift, base, authority_error}` |
| `org.apply_change` | Admin | `{agent_id, reports_to?, department?, expected_base}` | `{status:"applied", agent_id, before, after, base, registry_reloaded, withdrawn_pending:[id]}` |
| `org.propose_change` | Manager+ | `{agent_id, reports_to?, department?, reason?}` | `{status:"pending_approval", approval_id, agent_id, before, after, base, expires_at}` |
| `org.pending_changes` | Manager+ | `{agent_id?}` | `{count, pending:[{approval_id, agent_id, before, after, base, stale, proposer, proposer_email, reason, created_at, expires_at}]}` |

If you leave out a field, it keeps its current value. An empty string means top level or no department. Proposals are decided with the existing `approvals.decide {id, approve, reason?}` (approval kind `org_change`). Errors from rule checks are `{code, message}` with `code` one of `agent_not_found`, `self_report`, `target_not_found`, `cycle`, `invalid_department`, `unknown_department`, `no_change`, `root_requires_admin`, `pending_exists`, `stale_base`, `org_authority_unavailable`, `org_authority_write_failed`, `org_lock_unavailable`, `mirror_sync_failed`, `decision_unrecorded`, `already_applied`. A `stale_base` error also returns `current` and `current_base`. Permission refusals are a plain `permission denied`. `org.pending_changes` shows a Manager only the proposals they made and those for employees they may reorganise. A Viewer binding alone shows nothing.

See [Delegation isolation](37-delegation-isolation.md) for how the org chart governs delegation.
