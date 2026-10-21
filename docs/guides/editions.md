# Personal and Enterprise editions

DuDuClaw runs as the Personal edition by default and switches to the Enterprise edition when you install a license key of an enterprise plan. The two editions share the same core. This page lists what differs so you can tell which one fits.

Principle: the free core is not crippled. The editions differ in quantities and in management surfaces for several people, not in what an AI employee can do.

## Comparison

| Item | Personal | Enterprise |
|---|---|---|
| AI employees | No hard limit. The dashboard shows a gentle hint above 3 | No hard limit on self-hosted installs. A license may carry its own cap |
| Goal tasks running at the same time | 2, the rest wait in the queue | Unlimited |
| OS-native sensing employees | 1 | Unlimited |
| Member accounts, departments, role permissions | Not available (one owner) | Available |
| Distributor management, identity resolution, wiki trust governance, reliability report | Not available | Available |
| Local auto-login (no password on the machine itself) | Can be turned on | Not applicable |
| Paid industry templates | Included when the license includes them | Included |
| Runtimes, channels, memory, skills, MCP tools, approvals, sandboxes, computer use | All of them | All of them |

Notes:

- Personal-edition limits are defaults. They count work that is running or registered right now, they do not remove any feature.
- Local auto-login only works for a Personal install that listens on the local machine only. Behind a reverse proxy or on a network address it stays off.
- Cloud-hosted plans have their own quotas. This page covers self-hosted installs, where the edition follows the license key you installed.

## What decides the edition

The edition follows the installed license. With no license key you run Personal. Keys of the enterprise plans switch the dashboard and the server to Enterprise. Removing the key returns you to Personal and nothing is deleted.

## Upgrading

1. Get a license key from your authorised distributor (commercial licensing is not sold directly by the maintainers; see [LICENSING.md](../../LICENSING.md)).
2. Open the dashboard and go to **Manage → Advanced → License** (the command palette, ⌘K, finds it too). You can also open `/app/system/license` directly.
3. Paste the key and activate it. The page shows the plan, and the edition changes right away.

If an item on the Personal side shows a notice that it is available on the Enterprise edition, the same License page is where the upgrade happens.
