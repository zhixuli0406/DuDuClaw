# Department managers and main AI employee access

In **Members** (`/manage/users`), open **Department management grants** on a Manager row to choose the five groups; additions require confirmation. Revoking a grant leaves the Manager’s schedules running. Edit the main AI employee’s **Who may use** list on its employee edit page.

Available in Enterprise from v1.86.0. An Admin can give an active Manager selected controls over AI employees in a named department. The Manager's role or their own department label does not grant these controls automatically.

[繁體中文](zh-TW/79-department-managers.md) · [日本語](ja-JP/79-department-managers.md)

## Granting access

Start with nobody granted access. Choose the Manager, department and each group separately: schedules, automation rules, skills, usage and outbound-data review.

Before granting or widening access, the Admin must read the risk note and tick **I understand** (「我了解」). Removing access needs no acknowledgement. Every grant and revocation is audited. Grants are automatically revoked when the person stops being an active Manager, including a role change, suspension or deletion.

Department grants work only while signed in to the dashboard. Personal API keys and chat apps cannot use them. They do not grant access to conversations, memories, files or employee settings, or permission to chat with or assign ordinary work to an employee.

## What each group allows

### Schedules

| Can do within the department | Cannot do through this grant |
|---|---|
| View and manage routines, reminders and scheduled assignments. Create prompt-only routines; edit, pause, resume, delete and run routines now where the corresponding action is available. Cancel reminders; pause, resume or remove scheduled assignments. | Schedule the main AI employee, create command or workflow routines, or bypass existing input, employee availability and execution checks. The grant does not add creation or editing actions to reminder or scheduled-assignment lists. |

### Skills

| Can do within the department | Cannot do through this grant |
|---|---|
| Read skills, switch them on or off, remove and restore them, and request installation for an employee. An Admin must approve the installation. | Install directly, add or edit skill content, or publish or adopt shared skills. |

### Usage

| Can do within the department | Cannot do through this grant |
|---|---|
| Read usage per employee, totals over those employees and recent records. The department usage view offers 7, 30 and 90 days. | Read another department through this view or change budgets. |

### Automation rules

| Can do within granted departments | Cannot do through this grant |
|---|---|
| List, create, edit, enable, disable and delete department rules, and read their history. Work and skill actions may target only employees in that department. Notifications may go only to the rule owner's verified direct message or a department employee's notification target. | Change a rule's department; manage company-wide rules, circuit breakers or monitoring tick sources; use event sequences or automatically induced rules. Main and personal employees are excluded as targets. |

Department rules accept only three triggers: task created, task updated and task status changed. Only task changes made through the dashboard can trigger them; events reported by AI employees themselves cannot. New activity, channel messages, employee idle and run-at-risk events are unavailable. Actions requiring approval still need an Admin.

Department scope is checked when a rule triggers, before conditions, screening, approval or actions, and again before each action and after approval. Events from other departments are ignored silently. If a target or notification destination has left the department, the rule is skipped with “部門範圍不符，已略過” (department scope mismatch, skipped). Queued work is checked again before routing and immediately before execution, including whether the rule is still enabled for that department. The owner’s verified direct message qualifies only while the owner is an active Admin or an active Manager with the automation grant for that department; losing that eligibility removes the destination. Rule and recipient eligibility are also re-checked before run and approval notifications.

### Outbound-data review

An Admin can grant an active Manager outbound-data review for each department. In the dashboard only, that Manager may open and approve or reject outbound-data approval cards for AI employees in the granted department. They see the same full parameters as an Admin for that card. They cannot decide a request made on their own behalf. Personal employees are never covered by department grants, including a Manager’s own personal employee.

Lists, timelines and notifications show only a fixed summary without request content. An authorized reviewer opens the card view to see the full card. Every opening by a non-Admin must first be audited; if the audit cannot be written, opening is refused. Decisions record the reviewer type in the audit; decision audit writes are best-effort, and the approval record keeps the decision maker.

Card notifications go only to active Admins and active Managers holding this grant for the employee’s department. Before every send, including reminders, the recipient’s current role or grant and exact verified direct-message destination are checked again. A mismatch or unreadable information skips that recipient silently. Chat decisions remain Admin-only; this grant cannot be used through chat or personal API keys. See [Outgoing data guard](69-egress-guard.md).

## Which employees belong to a department?

The official org chart record must agree with the employee's own department settings. A mismatch grants no access and is shown as needing an Admin org sync. Changing an employee's own settings cannot expand a Manager's reach. The main AI employee and all personal employees are excluded from department grants. Owners retain access to their personal employees through their existing binding.

When an org chart move changes Managers' reach, the change summary says who gains or loses which controls. Approval checks that effect again. If it changed after the Admin reviewed it, the Admin must review and confirm again. Org sync also reports the effect.

## Revocation and existing work

Revoking a grant does not automatically pause or delete routines or rules the Manager created. Routines keep running under their existing execution checks; rules retain their saved enabled state and continue under the department checks above. Admins should disable unwanted work separately.

Routine and scheduled-assignment lists provide a **Creator no longer has access** (「建立者已無權限」) marker when that loss can be determined. The Automation page (`/routines`) displays this marker. An equivalent marker for automation rules is not yet provided.

## Members' read-only rule view

Signed-in dashboard members can read company-wide and department rules that assign work or run skills on employees they can view. The view includes conditions, visible targets, the prompt template or skill name, approval requirements and the latest 20 trigger times and results. It gives no editing rights. Notification destinations and message text, other employees and detailed history contents are omitted. In protected mode, a rule naming another person's personal employee is hidden entirely.

## Who may use the main AI employee?

The Enterprise main AI employee access list is empty by default, leaving access with Admins. An Admin may add active members to chat with it and hand it ordinary, non-scheduled work. Adding people requires reading the risk note and ticking **I understand**; changes are audited. Inactive or deleted accounts are removed from the list.

The list takes effect only in protected mode with isolated employee execution. Otherwise, only Admins can use the main AI employee through these entry points. The settings response explains why the saved list is inactive. Removing command tools does not activate it. Personal API keys can never use the main AI employee, even for a listed person or an Admin.

Listed people may be able to ask about what the main employee remembers, including its conversations with Admins. Consider this before adding anyone. Scheduling the main employee, creating employees and changing employee settings remain Admin-only. When the main employee works for a listed non-Admin person, administrator tools are unavailable; an attempt is refused with “This action requires the administrator to perform it personally.” (「這個動作需要管理員本人操作。」).

The main employee may still hand work to other employees. Those hand-overs follow its normal delegation rules. We recommend setting `[delegation] human_principal = "enforce"` so the person's access is also checked when work is delegated.

## Known limitations

- The main access list grants nothing outside protected execution. A saved list alone does not enable isolation.
- Company-wide analytics and billing summaries remain visible to all Managers. The department usage grant does not narrow those existing summaries.
- In **Members** (`/manage/users`), open **Department management grants** on a Manager row to choose the five groups; additions require confirmation. Revoking a grant leaves the Manager’s schedules running. Edit the main AI employee’s **Who may use** list on its employee edit page. Use **Automation** (`/routines`) for department rules and the member read-only tab. See [Dashboard settings](../guides/dashboard-settings.md).

## For developers

Department settings: `users.dept_grants.list`, `users.dept_grants.set`, `users.dept_grants.options`, `users.dept_grants.mine` and `users.dept_grants.reach_preview`. Main access: `agents.main_access.get` and `agents.main_access.set`. Department usage: `cost.department_summary` and `cost.department_recent`. Member rule view: `autopilot.affecting_me`.
