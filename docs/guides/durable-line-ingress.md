# LINE durable ingress and recovery

After a LINE webhook passes signature verification, the gateway writes the whole batch of events to SQLite and commits before it answers HTTP 200. Redeliveries, restarts and the different transport paths all share this one inbox record. Handling an event still goes through the existing reply, access control, chat command and tool gates.

## Check these two things first

1. **For a direct webhook, turn on webhook redelivery in the LINE Developers Console.** When the gateway cannot write to the inbox it answers 503 and expects LINE to resend later. Without redelivery, LINE does not resend after a 503 and the message is lost.
2. **The relay path does not give durable acceptance.** In deployments that go through `duduclaw-relay` (DuDuClaw OS and others), the relay answers LINE with 200 as soon as it receives the request and forwards it to the gateway afterwards. If the gateway then does not accept it (disk full, inbox cannot be opened, stop switch off, configuration unreadable), the event is gone and LINE will not resend it. The gateway counts these in `relay_frames_total{channel="line",outcome="not_accepted"}` (a bad signature is `bad_signature`) and writes an Activity Feed row `relay_line_rejected` (at most one per kind every ten minutes). Use the direct webhook when you need durable intake.

## Settings

```toml
[channel_ingress]
line_enabled = true          # stop switch, default true
line_late_reply = "push"     # "push" (default) or "fail"
line_workers = 8             # ordinary workers, 1–64, default 8; takes effect after a gateway restart
retention_days = 90          # days finished events are kept, minimum 1
stuck_alert_minutes = 15     # alert when a conversation is held back this long, 0 = off
capacity_alert_mb = 512      # alert when database + WAL exceed this many MB, 0 = off
```

Apart from `line_workers`, settings take effect the next time they are read; no restart is needed. When `config.toml` cannot be read or parsed, workers stop dispatching (as with `line_enabled = false`) and webhooks get 503.

### The stop switch `line_enabled`

With `false`: new webhooks get 503, events not yet started stay in `ready`, turns already running finish, but the switch is checked again before a reply or progress notice is sent, and nothing is sent while it is off. The inbox, the deduplication records and operator decisions are kept, and the 24-hour payload cleanup keeps running. When it is turned back on, events that are still valid are dispatched.

This switch **does not bring back the pre-v1.69 intake**; turning it off stops LINE. Differences between durable intake and the old behaviour:

| Item | v1.69.x and earlier | Now |
|------|------|------|
| When 200 is returned | Right after verification, handled in the background | After the batch is written to SQLite and committed |
| Process dies mid-way | Message lost | Accepted events continue after restart; possibly executed ones become `uncertain` |
| Several messages in one conversation | Run in parallel | Handled one at a time in arrival order |
| Conversations handled at once | Unbounded | `line_workers` (default 8) plus one decision worker |
| Reply token expired | Push instead | Per `line_late_reply`; default is still Push |
| LINE "Verify" without configured credentials | 200 | 503 |
| `config.toml` unreadable | Handled anyway | 503, dispatch paused |
| Chat commands and attachments belong to | The main AI employee | The AI employee this message is routed to |

### Late replies `line_late_reply`

LINE's Messaging API reference says a reply token can be used once and must be used within one minute of receiving the webhook. The gateway counts 60 seconds from the earlier of its own receipt and the event's `timestamp`. When an event waits in the queue or the turn itself is long, that window passes.

A **redelivered** webhook (`deliveryContext.isRedelivery = true`, only sent when webhook redelivery is on) is treated as already past the window: LINE says its token works except when the original delivery already used it or 20 minutes have passed, and the gateway cannot tell whether the first delivery reached it. Its token is never tried; the answer follows `line_late_reply`. If the Reply API answers HTTP 400 with the message `Invalid reply token` (used or expired), with `"push"` the answer goes by Push after a fresh check; with `"fail"` the event becomes `undelivered` with reason `reply_token_invalid`. Other refusals stay `undelivered` (`reply_rejected`).

- `"push"` (default): after the window, the answer is sent with the Push API to **the event's own conversation** (group, room or one-to-one user) and nowhere else. Before sending it gets the same checks as a reply: the dispatch lease is still held, the stop switch is still on, and the account credentials and route/authority snapshot have not changed. The request carries `X-Line-Retry-Key`; the result (including LINE's `x-line-request-id`) goes into the attempt receipt with `delivered_via` = `push`. Push uses the official account's message quota; once a free or light plan runs out LINE answers 429 and the event becomes `undelivered`.
- `"fail"`: the final answer is never switched to Push. If the window has already passed when a worker picks the event up, **the turn is not run**: the event becomes `failed_before_dispatch` with reason `late_reply_expired` (`redelivered_reply_token_not_used` for a redelivered webhook) and the operator is notified. If the window passes while the turn runs, the answer is not sent and the event becomes `undelivered`. A turn that started in time still sends its progress notices and approval/decision cards with Push, so Push use is lower but not zero. The cost is that users get no answer to longer turns.

## What acceptance covers

An event must have a LINE `destination` and a `webhookEventId`. All events of one signed envelope are written in one `IMMEDIATE` transaction; any failure rolls back the whole batch. SQLite runs with WAL, `synchronous=FULL` and a five-second busy timeout. A 200 means the batch was durably accepted; whether the model, tools or delivery succeeded is shown by the event's state.

Before the 200 only what verification and the write need is checked: one read of `config.toml` for the credentials and the stop switch, signature verification, envelope parsing, the write. An unreadable employee configuration, a broken `agent.toml` of some employee or an invalid channel setting value does not make the webhook answer 503. The event is stored with a marker of the credentials its signature was checked with, and the route/authority snapshot is taken right after the commit. **No worker picks an event up until its snapshot is stored.** A snapshot that cannot be read is tried again with back-off (5, 10, 20, 40 seconds); after 5 failures, or when an event without a snapshot is more than 5 minutes old (for example the gateway was down in between), the event becomes `quarantined` with reason `snapshot_unavailable` rather than adopting a later configuration silently. It never ran and can take a `retry`; the retried event is snapshotted with the configuration current at that time. A changed credential or a removed route at snapshot time quarantines it as `account_route_authorization_changed`.

The stable deduplication key is a length-prefixed SHA-256 of `channel + account(destination) + webhookEventId`.

A first run's `run_id` equals the `ingress_id`. An explicit `retry` or `rerun` stores a new authorization UUID as the new `run_id` in the same transaction; the source `ingress_id` does not change. An old worker cannot commit a receipt for the new run.

## Route and authority snapshot

Before dispatch, before the reply and before every progress notice, the snapshot is computed again and compared with the one stored with the event. It covers only "who handles this message, with which rights":

- Route: the resolved employee, its trigger/role/status, `[channels]`, `allowed_channels`, `default_agent`, and this user's employee binding.
- Authority: the LINE credentials digest; the routed employee's effective configuration (`agent_resolved/<id>.toml` for an employee bound to a job preset, otherwise `agent.toml`): identity fields, capabilities, permissions, budget, container sandbox/network; this conversation's and the global allowlist, blocklist, pairing, admin, mention-only and binding settings; this user's pairing state.

Adding an employee or editing an unrelated employee does not change the snapshot. Prompt text, SOUL, heartbeat and statistics do not either.

Two outcomes are kept apart:

- **Read and different** (rebinding, credential rotation, changed rights, the employee removed): the event becomes `quarantined` with reason `account_route_authorization_changed`; an old message is never handled with a new account or by a new recipient.
- **Could not be read or parsed** (a configuration being rewritten, a busy database): the event goes back to `ready` and is retried with back-off (5, 10, 20, 40 seconds); only after 5 consecutive failures does it become `quarantined` with reason `revalidation_unavailable`. Such an event never ran and can be retried.
- Before the final answer or a progress notice is sent, an unreadable snapshot is read again a few times over about seven seconds. If it still cannot be read, nothing is sent and the event becomes `undelivered` with reason `revalidation_unavailable`; it is never recorded as "authority changed" (`authorization_changed_before_delivery`), which is kept for a snapshot that was read and differs.

## States and operator actions

| State | Meaning | Holds back later messages of the conversation | Actions |
| --- | --- | --- | --- |
| `ready` | Accepted, waiting (possibly in back-off) | Yes | |
| `claimed` | 90-second lease taken, not started; returns to `ready` when it expires | Yes | |
| `dispatching` | Turn running, lease renewed every 20 seconds | Yes | |
| `completed` | Turn finished and LINE accepted the reply or Push | No | |
| `failed_before_dispatch` | Proven not executed (e.g. `late_reply_expired`) | No | `close`, `retry` |
| `undelivered` | Turn ran, answer not delivered (LINE refused it, authority changed before sending, late with "fail") | No | `close`, `rerun` |
| `uncertain` | Turn may have run, no delivery receipt (process gone mid-turn, connection lost while sending) | Yes | `close`, `rerun` |
| `quarantined` | Route/authority changed, configuration or snapshot unreadable repeatedly, payload expired, or restored from a backup | Yes | `close`; also `retry` when the reason is `revalidation_unavailable` or `snapshot_unavailable`, `rerun` when it is `restored_from_backup` |
| `closed` | Closed by an operator, or by the system after retention (`retention_closed`) | No | |

The inbox was never in a released version before v1.69. Databases made by preview builds are migrated: their `failed` state becomes `undelivered` (all those events had run), and `ready` events stored with an older snapshot format are quarantined as `account_route_authorization_changed` after the upgrade and can only be closed.

Actions:

- `close`: stop here; the record is kept, the message text and reply token are deleted at once.
- `retry`: only for events proven not executed; queues the event again.
- `rerun`: for events that may have run or did run (`uncertain`, `undelivered`, and `quarantined` after a restore). **The user may get a duplicate answer, tools may run twice, tasks may be created twice.** Requires `confirm_duplicate_risk=true` and a reason; the confirmation, the reason and the provider receipt you checked (leave it out if there is none) are stored with the new run.

The reply token's window counts from when the event arrived; a new run does not extend it. So:

- `line_late_reply = "push"`: the answer of a `retry` or `rerun` always goes by Push to the original conversation.
- `line_late_reply = "fail"`: for an event past its original reply window, `retry` and `rerun` are refused with "The reply window has passed and Push is not allowed (line_late_reply = "fail"); a new run would have no answer. Set line_late_reply to "push" to deliver, or close the event." (shown in Chinese). This avoids running a whole turn (tools, cost) whose answer cannot be delivered. If the setting is changed to "fail" after the decision, the worker still treats the run as late and does not execute it.

Every action passes the original account/route/authority snapshot check again; a changed snapshot still quarantines.

### Alerts

When an event becomes `uncertain`, `quarantined`, `undelivered` or `failed_before_dispatch` (late), a conversation's oldest waiting message waits longer than `stuck_alert_minutes`, the database passes `capacity_alert_mb`, or events are held after a restore, the gateway queues an alert in the inbox database. Every 30 seconds the queue is summarized per kind and reason over ten-minute windows:

- one Activity Feed row per kind, reason and window (`channel_ingress_uncertain`, `channel_ingress_quarantined`, `channel_ingress_undelivered`, `channel_ingress_late_reply_failed`, `channel_ingress_stuck`, `channel_ingress_capacity`, `channel_ingress_restored_held`) with the count and the first five event ids; alerts arriving later in the same window are summed into one follow-up row when the window closes;
- at most one notice per kind every ten minutes to the operator through the main AI employee's `[proactive]` notification target.

Window and push markers are stored in the inbox database, so a restart neither repeats a row nor resets the push limit. Alerts carry the first 12 characters of event ids, the state and the reason code only; never message text, a LINE user id or a token. They point to the terminal commands below and the dashboard's pending approvals; **there is no inbox page in the dashboard yet** (see "Known limitations").

### Dashboard RPCs (Admin)

- `channel_ingress.list`: up to 200 events per page with state, reason, back-off time, counts per state, DB/WAL bytes, the current settings and recent attempts and resolutions. `before_seq` pages back. No payload or reply token is returned.
- `channel_ingress.inspect`: one event's attempts (with `provider_receipt`, `delivered_via`, progress push counts) and run authorizations (with `action`, `confirmed_duplicate_risk`). For decision events the stored request id is used to read the approval record without changing it.
- `channel_ingress.resolve`: `ingress_id` (64 hex digits), `expected_revision`, `expected_attempt`, `action` (`close`/`retry`/`rerun`), `note`, optional `provider_receipt`, and `confirm_duplicate_risk=true` for `rerun`. Every call, accepted or refused, writes a security audit row `channel_ingress_resolution`.

All three re-read the user database on every call to confirm the caller is an Admin, and share the LINE worker's database connection.

### Terminal

```bash
duduclaw ops channel-ingress list
duduclaw ops channel-ingress show <ingress_id>
duduclaw ops channel-ingress resolve <ingress_id> --note "reason" [--retry] [--provider-receipt <id>]
duduclaw ops channel-ingress rerun <ingress_id> --note "reason" --confirm-duplicate-risk [--provider-receipt <id>]
duduclaw ops channel-ingress batch --action close|retry|rerun --status <state> [--reason <code>] --note "reason" [--confirm-duplicate-risk] [--limit 200]
```

An AI employee running `duduclaw` / `duduclaw-pro` `ops channel-ingress` from Bash is stopped by the agent-file-guard hook (`BlockedOperatorCommand`). The command is read the way bash reads it (line continuations, quotes joined to a word, a redirect stuck to a word, `env`/`npx` prefixes, absolute paths). That is a speed bump only: a variable, an alias, a script or a renamed binary gets past it, and non-Claude runtimes do not run the hook. The real gate is the dashboard approval below.

`list` and `show` answer directly. The first run of `resolve` or `rerun` **does not take effect**: it files an approval request, prints "請到儀表板的待辦核准" (go to the dashboard's pending approvals) with the approval id and exits non-zero. The terminal cannot tell the operator from an AI employee with Bash, so these changes always need an Admin's approval in the dashboard; a reply in a channel cannot approve them. Within 30 minutes of the approval, running **the same command** again (same reason and receipt) applies it; each approval can be used once, and a change of the event's state in between voids it. At most 20 terminal requests may wait at once. If the event's state changes while a request waits, the waiting card is withdrawn and the next run files a new one (a card is never rewritten in place). Every request, application and refusal writes a security audit row `channel_ingress_cli_action`. A process carrying an AI employee's session variables is usually refused, but removing those variables gets past that check; the dashboard approval is the gate. For urgent cases use the dashboard RPCs.

`batch` handles many events with one approval: it selects the events in one state (`uncertain`, `quarantined`, `undelivered` or `failed_before_dispatch`), optionally with one reason code, that the action applies to, at most `--limit` (default 200, at most 500), and files one approval bound to the action, the selection, the reason, and every selected event's id with its state version. After an Admin approves it, running the same command again applies it once: events whose state has not changed are resolved, events that changed in between are skipped, and the output lists `applied`, `skipped_changed` and `failed`. If the selection changes before the decision, the waiting card is withdrawn and a new one filed. `rerun` in a batch needs `--confirm-duplicate-risk` like a single rerun.

`list` and `show` print LINE account and conversation ids as a short digest (`#` and 12 hex characters), so events of one conversation still line up without showing a LINE user or group id.

## What operators should know

**Ordering.** Within one account and one conversation, an ordinary message waits while any earlier message is `ready`, `claimed`, `dispatching`, `uncertain` or `quarantined` (a `ready` event in back-off counts). `completed`, `closed`, `undelivered` and `failed_before_dispatch` do not hold anything back, so once a message ends `undelivered` or `failed_before_dispatch`, the later messages of the conversation are handled. A `retry` or `rerun` returns the event to `ready` in its original arrival position: it runs before every message of the conversation that has not started yet, but later messages that were already handled are not redone, so the answer of a new run can arrive after the answers to later messages. While an event is `uncertain` or `quarantined` the later messages wait, so for those two states the order stays correct after `rerun`/`retry`. Decision messages (approve, deny, answer) neither wait nor hold anything back, and conversations do not affect each other.

**Restoring from a backup.** Device backups (`device.backup_create`, scheduled backups, `duduclaw export`) all include `channel_ingress.db`, and the device the backup came from may have handled its waiting messages after the backup was taken. So the device restore (`device.backup_restore`) writes a one-time marker **before it moves any data**; if the marker cannot be written the restore stops with that reason and nothing is swapped. The next time the gateway opens the inbox, before any worker can pick anything up, every event that was `ready`, `claimed` or `failed_before_dispatch` in the backup, or `quarantined` with a retryable reason (`revalidation_unavailable`, `snapshot_unavailable`), becomes `quarantined` with reason `restored_from_backup`, and the marker is deleted. These events never ran on this machine but may have been handled, or retried, on the old one, so they cannot take a `retry`: close them, or `rerun` them after confirming the duplicate risk. The gateway writes an Activity Feed row (`channel_ingress_restored_held`) and notifies the operator with how many were held. Events that were `dispatching` in the backup become `uncertain` and are not rerun automatically either. Unpacking a `duduclaw export` archive onto a new machine by hand leaves no marker; in that case turn LINE off first (`line_enabled = false`) and turn it back on after checking.

## Progress notices and document notices

Progress notices (at most one Push per minute during a long turn, sent to the sender's one-to-one chat even when the message came from a group) are secondary. They are sent whatever `line_late_reply` says. Each send is revalidated; results are counted apart in the attempt receipt, a failure never turns a successful turn into `uncertain` and never holds up the conversation. When the turn ends the gateway waits at most 10 seconds for the results; any that have not come back count as unknown.

The 📎DELIVER notice ("the file is ready, download it from the dashboard") is not pushed separately. It is appended to the answer of this turn and goes out through the same reply (or late Push) path with the same revalidation and receipt.

## Retention and capacity

The original single-event JSON (including the reply token) is kept in a separate `ingress_payload` table for at most 24 hours and deleted at once when the event is `completed` or closed. A waiting event whose payload expires becomes `quarantined` (`payload_retention_expired`) and can only be closed. The payload of an `undelivered` or `failed_before_dispatch` event goes after 24 hours too, so after that it can no longer take a `retry` or `rerun`, only `close`.

Finished events (`completed`, `closed`), with their attempts, receipts, run authorizations and resolutions, are deleted after `retention_days` (default 90). `undelivered` and `failed_before_dispatch` events hold nothing back and are closed by the system after `retention_days` (reason `retention_closed`) and deleted with them. LINE redelivers within hours, so deletion after 90 days does not affect deduplication. `uncertain` and `quarantined` events, which hold their conversation, are never deleted automatically. When database + WAL exceed `capacity_alert_mb` an alert is raised once a day. When storage runs out the sender gets 503; records still needed for deduplication are not deleted.

The database file is 0600; opening refuses symbolic links and never opens and closes an existing database file (that would drop this process's POSIX locks on it). SQLite `secure_delete` and WAL checkpoints clear deleted content from the live database; backups and external copies are not covered.

## Known limitations

- **No inbox page in the dashboard.** Events are viewed and handled in the terminal (`list`, `show`, `resolve`, `rerun`, `batch`) with the approval in the dashboard's pending approvals, or through the Admin RPCs. After an incident with many events, handle them batch by batch.
- `X-Line-Retry-Key` deduplicates retries within one run only. A `rerun` is a new run with new keys, so after `push_delivery_uncertain` LINE cannot tell the rerun's Push apart from the first one; check the provider receipt before rerunning.
- When Push quota runs out (HTTP 429) the event becomes `undelivered`; the answer is not stored, so sending it later means a `rerun` of the whole turn (model and tools again).
- `line_late_reply` and the other settings are cached by the file's modification time and size. A rewrite within the same timestamp tick that keeps the length (`"push"` ↔ `"fail"`) can be read late on file systems with coarse timestamps; the next edit or a restart picks it up.
- An empty answer completes the event without sending anything (unchanged behaviour).

## What has been verified

Local tests cover: `retry`/`rerun` of a late event refused under "fail", and a rerun approved before the setting changed to "fail" neither runs nor sends any request; the Bash lane stopping `ops channel-ingress`; a restore marker holding waiting events before any worker claims them, once; no 200 on bad signatures, batch rollback, full disk, read-only database or busy database; 20 concurrent deliveries of one event run once; queue-induced lateness under both settings ("push" sends to the same conversation, "fail" runs nothing and sends nothing); a failed progress push leaves the event's state alone; document notices joining the answer; preset-resolved configuration in the snapshot; unrelated employees, changed or broken, affecting neither the snapshot nor the acknowledgement; back-off instead of immediate quarantine when configuration cannot be read; a real OS kill through the webhook handler and the worker (after acknowledgement, during a turn); terminal changes needing a dashboard Admin approval that is used once and voided by a state change; an event not claimable until its snapshot is stored, an unreadable snapshot backing off and an event left without a snapshot across an outage quarantined as `snapshot_unavailable`; a redelivered webhook never trying its reply token (Push under "push", not run under "fail"); a refused reply token falling back to Push under "push" and staying `undelivered` under "fail"; an unreadable check before sending recorded as `revalidation_unavailable`, a changed one as `authorization_changed_before_delivery`; 500 alerts becoming one Activity row with the count, with no repeat after a restart; a batch applying only to unchanged events and a changed selection refiling its card; terminal output without raw LINE ids; a restore stopping before moving data when the marker cannot be written; continuation, quote-joined and redirect-stuck spellings of the command caught on the Bash lane.

Not yet verified on a real LINE account: redelivery, the actual reply-token window, the exact error body LINE returns for a used or expired token (the gateway matches the message `Invalid reply token`; any other 400 stays `reply_rejected`), the response when Push quota runs out, deduplication by `X-Line-Retry-Key`, and relay upstream behaviour. Local tests use a fake provider.
