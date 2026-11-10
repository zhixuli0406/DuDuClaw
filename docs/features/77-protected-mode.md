# Native Linux protected mode (planned for v1.82.0)

> **Status: planned for v1.82.0, not available in v1.81.0.** The code is included in v1.81.0, but it cannot be installed or enabled on a customer host yet.
>
> What v1.81.0 does instead: `sudo duduclaw service install --protected-linux` installs the single-binary protected service as in v1.80, and `sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux` updates it. AI employees are created and run exactly as in v1.80. See [Single-binary protected service](../guides/linux-protected-service.md#single-binary-protected-service-current-releases).
>
> Everything below (the Settings management page, Personal and Shared employees, per-employee connector accounts, retire and restore, the migration from v1.79.3) describes the planned release.

Protected mode runs every AI employee under its own account in its own execution environment on a native Linux host. Conversations, memory, files, connections and permissions stay with that employee, and the gateway's own account is never lent to it. When the isolation service or a trusted policy is missing, the employee's work is refused instead of falling back to the gateway account.

The feature is off by default and only available in the Enterprise edition. Installations that do not use it behave as before. It has not yet been accepted on real Linux hosts (see [What still needs a real host](#what-still-needs-a-real-host)).

## Turning it on (planned for v1.82.0)

These steps apply once the isolation components can be installed. On v1.81.0 they stop at step 1: the installer sets up only the single-binary protected service, readiness does not report the isolation as ready, and the management page does not appear.

1. Install the protected service on a supported host (Ubuntu 22.04, Ubuntu 24.04 or Debian 12; x86_64 or ARM64) with `sudo duduclaw service install --protected-linux`. Steps are in the [protected service guide](../guides/linux-protected-service.md).
2. Run `sudo duduclaw service readiness --protected-linux`. In the dashboard, the readiness banner names what blocks protected work (account or connection setup unfinished, authorization revoked, service temporarily unavailable) and shows a support reference.
3. When system status reports that protected mode is ready, Admins see **Settings › Protected mode**. It has four tabs: AI employees, Connector accounts, Google tenant source and Records.
4. Existing employees come over through the Admin migration confirmation page. Each proposal is approved, rejected or retried after a password and two-factor check; low-risk proposals can be handled in a batch.

Before a host is accepted, run the [acceptance runbook](../guides/linux-protected-acceptance.md).

## Personal and Shared employees

| | Personal | Shared |
|---|---|---|
| Belongs to | One staff member, the owner | One tenant |
| Who can use it | The owner only | Tenant members, with their usual role |
| How it is created | The staff member requests it under **My account** (up to 3 pending requests each); an Admin prepares it after a password and two-factor check and confirms it on the approval card. An Admin can also propose it directly | An Admin proposes it and confirms it on the approval card |
| Connections | The owner's own accounts | Tenant accounts, plus permitted home connections |
| Background work (schedules, reminders, periodic checks) | Not available | Available after an Admin approves it (decision within 24 hours, valid up to 30 days) |
| Being called or assigned by others | Others cannot assign work to it, Admins included; calling it with `@` works only when the owner turned on public calling | Follows the usual delegation rules |
| Data visibility for Admins | Basic information only | Normal |

Creating an AI employee directly is not possible in protected mode; every employee comes from a request or proposal and an approval.

### Who sees a Personal employee's data

Conversations, execution records, memory, configuration files, the employee's Wiki, task content, files and approval content belong to the owner. Managers do not gain access through the people they manage. An Admin who is not the owner can read this data only after re-verifying with password and two-factor code (valid for 5 minutes), and every read is recorded with the Admin, the employee, the owner and the data category. Cross-employee lists and searches leave these employees out, and other AI employees cannot change, remove or read a Personal employee's settings, skills, Wiki or mailbox.

## Retire, restore, adopt, purge and release

| Step | What happens |
|---|---|
| Retire | An Admin retires a Personal or Shared employee. It stops at once, running work is aborted, its connector accounts are revoked, and its data is sealed in quarantine for 90 days. The employee name stays reserved for those 90 days. |
| Restore (Personal) | The owner requests it with the strongest second factor they had before retirement (a failed check counts toward login lockout). An Admin approves after a password and two-factor check, and the approver must be a different qualifying Admin than the one who filed it. When the only qualifying Admin is the requester, they may approve their own request after a 7-day cooling period, and the approval is recorded. An owner who has no second factor cannot file; an Admin files for them. A password, email or two-factor change made by the owner counts as theirs; a change by anyone else blocks the restore. Revoking the owner's sessions after retirement does not block a restore. |
| Restore (Shared) | An Admin files the request, picks members from the roster at retirement time (permissions can stay or go down, never up; disabled or departed members are skipped and listed) and picks which tenant connections to approve again. A second Admin approves. Only the picked members and connections take effect. Other members must bind again, employee-specific connections must be set up again, and schedules and tasks stay paused. Each restored member is notified. |
| After any restore | Website credential grants and reporting relationships with other employees are listed for the Admin to authorize one by one. They do not come back automatically. |
| Adopt by a new employee | Once the host confirms the old employee's resources are gone, the owner (Personal, only to themselves) or an Admin (Shared, only within the same tenant) can start a new employee from the retired one. Only SOUL.md, CONTRACT.toml, agent.toml, SKILLS and a filtered configuration (deny and ask lists, model) come over. Memory, conversations, tasks and schedules stay in quarantine, the old employee's tokens and grants stop working, and the host workspace is not copied. If it fails midway, files return to quarantine and are verified again. |
| Purge | After 90 days an Admin purges the quarantine after a password and two-factor check. Before 90 days the Admin ticks a confirmation and files an early purge that a second Admin approves; the contents are compared again at approval and a change needs a new request. Purging deletes quarantine files, the employee's memory, conversations, schedules, tasks and retained website credential grants. Data already taken over by a new employee is untouched. |
| Release host resources | After a purge, an Admin can release the host side: the host workspace is erased, the account disabled and the employee code freed. The UID is never reused. Once started, a release cannot be cancelled, only retried to completion. |

Owners and Admins are notified when requests are filed and when a purge runs (at most 5 notices per owner or tenant per day). The audit log keeps an archive digest, file count and size. A purge that is interrupted finishes at the next action or restart. When the host cannot be reached, records disagree or work is still running, the step is refused or retried later.

**First 7 days after upgrade.** Approval eligibility is judged from 7 days of Admin status records, and that log starts at the upgrade. For 7 days no restore, purge or release can be approved.

## What a protected employee can do

- Run programs, analyze code and run Office scripts inside its own environment. The result comes back in the same call. Cancelling or losing the connection ends only that call.
- Read files from its own authorized workspace.
- Parse documents, spreadsheets, images, web pages and expert ZIP packs in a separate resource-limited service. Output is verified before it is returned.
- Fetch and extract public web pages only when an Admin sets `public_web = true` under `[isolation]` in the employee's `agent.toml` (default off, Admin-only, changed in the file or the Admin raw editor). Only public HTTPS sites are reachable; IP addresses, credentials in URLs, non-default ports, internal names and redirects are refused.
- Install a skill from the skill hub (download through the protected public path, unpack in the isolated parser, scan, then the existing approval) and synthesize skills from its own events with its own model settings and budget. Skills land only in its own skills folder and never overwrite one with the same name. Publishing to the global skill bank needs an Admin approval card.
- Hand work to another employee or start a temporary helper. Each call is checked against the employee's own authorization. A Personal employee's work for a Shared employee needs its owner's approval. A temporary helper runs as the original employee, with only the tools of the original run, after that run ends, and expires after 24 hours if not started.
- View and withdraw discovery requests it filed itself, and view or compare its own branch trials. Creating, selecting and ending branch trials comes in the next release; cost shows "unavailable" until measured, never zero.

## Browser recording (off by default)

Recording is gated until the host proves that Chromium starts inside the helper sandbox. Until then readiness shows `helper_recording_unsupported` and the recording tools and "distill a skill from a recording" stay hidden. The employee also needs `public_web` turned on. When recording is available, the artifacts carry a hash recorded at the end of the run, a changed file is refused on read, and the raw trace can hold sensitive screen content. A stop request does not mean the recording has finished.

## Upgrade notes

- On Personal employees, old schedules and reminders without a recorded creator stop when they fire. The owner can create them again.
- Background work approved before this version needs a new Admin approval; one request is filed automatically.
- Admins lose implicit access to other people's Personal employees.
- Live log tail is unavailable while hidden Personal employees exist.
- Discovery requests filed before the upgrade are not visible to the employee (Admins still see them).
- Recording is off by default.

## What still needs a real host

Component tests ran as root on Ubuntu 22.04, Ubuntu 24.04 and Debian 12, but the full flow has not. Open items: the acceptance runbook on each distribution (including the Landlock ABI 8 kernel on Ubuntu 24.04 with kernel 7.0), real provider accounts for Google, Notion, GitHub and Odoo, the Chromium probe that unlocks recording, and the root-only checks for retire and release. Do not rely on protected mode for production isolation before those pass.

Developer references: [tool matrix](../architecture/native-linux-isolation-tools.md), [implementation notes](../architecture/native-linux-isolation.md).
