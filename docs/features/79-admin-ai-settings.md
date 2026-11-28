# Administrator AI settings

An administrator can ask their own AI avatar to read and change everyday settings for multiple AI employees at once. The feature is off by default. An administrator enables it in **Settings → Advanced → Delegation permissions**, in the **Administrator AI** card at the bottom after reading the risk explanation and acknowledging it; the system records who enabled it and when.

Turning the switch on requires **I understand**; turning it off takes effect immediately. In **Inbox → Administrator AI settings change confirmation**, the card shows a ten-minute countdown; processing results distinguish done, partial and voided. Processed cards show the decision maker and decision time when available.

## Who can use it

Only an active administrator who has changed the initial password can use the feature, through their uniquely assigned AI avatar, while personally present in that conversation. Reading and proposing changes are available in private chats and signed-in WebChat. Group chats are read-only: schedule prompt text and skill descriptions are omitted. Protected mode does not provide this feature. Scheduled, delegated and background turns never qualify.

Targets are visible employees, excluding terminated employees, temporary team roles and another person's personal employee.

## What can change

The administrator's AI can propose changes to display name, icon, mention name, active or paused status, reply language, primary and fallback models, reasoning effort, run time limit, maximum steps, idle timeout, scheduled wake-up settings, prompt-only time schedules, and enabling or disabling installed skills. Models must run on the employee's current engine. Time and step limits stay within the global limits. Scheduled wake-ups support on/off, time, interval and time zone. Schedules cannot run workflows or be started immediately through this feature.

These settings remain dashboard-only:

| Setting | Why it stays in the dashboard |
|---|---|
| Tool and data access permissions | They determine what an AI can do. |
| Employee operation permissions and available channels | They protect delegation, employee creation, schedules and skill operations. |
| Sandbox, network isolation and file protection | They change execution isolation and file safeguards. |
| Execution engine, fallback engine and background services | Engine changes alter tools and isolation and may send conversations to another provider. |
| Model accounts and budget increases | They affect cost and payment limits. |
| Service identities, channel credentials and public access | They control access on behalf of colleagues and exposure outside the company. |
| Managers, departments, employee IDs and primary roles | They change organizational and delegation authority. |
| Delegation scope, external agent connections and authentication | These can widen delegation or remove identity checks. |
| External tool servers and connection settings | They change external tools or executable connections. |
| Employee personality and work contract | These have a separate dashboard approval flow. |
| Archiving and deleting employees | These actions may be irreversible. |
| Installing skills from outside | External code has supply-chain risk; the AI may only enable or disable installed skills. |

## Review and apply

Every proposed change, including a single change, creates a confirmation card showing each employee, setting, previous value and new value. The card is valid for ten minutes. Only the administrator who proposed it can confirm or cancel it, in the dashboard inbox; chat confirmation is not available in this version.

If any affected employee's settings change before confirmation, the entire batch is voided. Once confirmed, execution stops at the first failure, does not roll back earlier changes, and reports what was completed.

## Audit

The security audit log records proposals, changes, voided or refused requests, and switch changes. Dashboard changes to ordinary employee settings also record the actor and before-and-after values. Administrators can review these events in the dashboard's security audit log.
