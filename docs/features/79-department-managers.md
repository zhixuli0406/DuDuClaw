# Department managers and main AI employee access

Available in Enterprise from v1.86.0. An Admin can give an active Manager selected controls over AI employees in a named department. The Manager's role or their own department label does not grant these controls automatically.

[繁體中文](zh-TW/79-department-managers.md) · [日本語](ja-JP/79-department-managers.md)

## Granting access

Start with nobody granted access. Choose the Manager, department and each group separately: schedules, automation rules, skills and usage. A fifth option, outbound-data review, also exists; its operation will be described on the outbound-review page when that page is available.

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

Allowed triggers are: a task is created, a task is updated, a task's status changes, new activity is recorded, a channel message arrives, an employee becomes idle, or a run is at risk. Other triggers are unavailable for department rules. Actions requiring approval still need an Admin.

Department rules can be accepted and stored before they can run. See the execution limitation below.

## Which employees belong to a department?

The official org chart record must agree with the employee's own department settings. A mismatch grants no access and is shown as needing an Admin org sync. Changing an employee's own settings cannot expand a Manager's reach. The main AI employee and other people's personal employees are never included; department automation targets exclude personal employees altogether.

When an org chart move changes Managers' reach, the change summary says who gains or loses which controls. Approval checks that effect again. If it changed after the Admin reviewed it, the Admin must review and confirm again. Org sync also reports the effect.

## Revocation and existing work

Revoking a grant does not automatically pause or delete routines or rules the Manager created. Routines keep running under their existing execution checks; rules retain their saved enabled state and remain subject to the execution limitation below. Admins should disable unwanted work separately.

Routine and scheduled-assignment lists provide a **Creator no longer has access** (「建立者已無權限」) marker when that loss can be determined. The gateway supplies this marker; its dashboard display still needs integration. An equivalent marker for automation rules is not yet provided.

## Members' read-only rule view

Signed-in dashboard members can read company-wide and department rules that assign work or run skills on employees they can view. The view includes conditions, visible targets, the prompt template or skill name, approval requirements and the latest 20 trigger times and results. It gives no editing rights. Notification destinations and message text, other employees and detailed history contents are omitted. In protected mode, a rule naming another person's personal employee is hidden entirely.

## Who may use the main AI employee?

The Enterprise main AI employee access list is empty by default, leaving access with Admins. An Admin may add active members to chat with it and hand it ordinary, non-scheduled work. Adding people requires reading the risk note and ticking **I understand**; changes are audited. Inactive or deleted accounts are removed from the list.

The list takes effect only in protected mode with isolated employee execution. Otherwise, only Admins can use the main AI employee through these entry points. The settings response explains why the saved list is inactive. Removing command tools does not activate it. Personal API keys can never use the main AI employee, even for a listed person or an Admin.

Listed people may be able to ask about what the main employee remembers, including its conversations with Admins. Consider this before adding anyone. Scheduling the main employee, creating employees and changing employee settings remain Admin-only. When the main employee works for a listed non-Admin person, administrator tools are unavailable; an attempt is refused with “This action requires the administrator to perform it personally.” (「這個動作需要管理員本人操作。」).

The main employee may still hand work to other employees. Those hand-overs follow its normal delegation rules. We recommend setting `[delegation] human_principal = "enforce"` so the person's access is also checked when work is delegated.

## Known limitations

- Trigger-time department checks arrive with this version's automation update. If that update is not active on a gateway, department rules are skipped, never run outside the department. The current department implementation skips them before evaluating conditions or taking actions, including after approval. Saving a rule does not prove it will run.
- The main access list grants nothing outside protected execution. A saved list alone does not enable isolation.
- Company-wide analytics and billing summaries remain visible to all Managers. The department usage grant does not narrow those existing summaries.
- The routines, skills and department usage pages support grants. The Members-page grant editor, the main employee's **Who may use** control and the department-rule/member-rule screens still need dashboard integration. The gateway already supports their permissions and settings. See [Dashboard settings](../guides/dashboard-settings.md).

## For developers

Department settings: `users.dept_grants.list`, `users.dept_grants.set`, `users.dept_grants.options`, `users.dept_grants.mine` and `users.dept_grants.reach_preview`. Main access: `agents.main_access.get` and `agents.main_access.set`. Department usage: `cost.department_summary` and `cost.department_recent`. Member rule view: `autopilot.affecting_me`.
