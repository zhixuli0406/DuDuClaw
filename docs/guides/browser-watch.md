# Watching an employee's browser

Only a live, verified human turn establishes the session’s origin. A scheduled or delegated background run does not become a human start by carrying that person’s identity. The admitted origin remains recorded after the turn ends; it grants no continued presence or browser control.

Open `/computer/<employee>` while signed in to the dashboard. This page does
not grant access to the employee's settings, files, memory, or conversations.

An existing direct binding keeps its normal live-view controls. A manager
reaching an employee through a reporting line sees masked snapshots and can
end that specific session. Team access grants no raw stream, takeover,
hand-back, injection-hold release, or login-state reset.

Team viewing requires Enterprise and the same `[people] manager_team_history`
setting used by team conversation history. The session must have been started
by a qualifying report after the current activation window began. A shared
employee binding does not expose another person's session. Access is checked
again for every request; turning the setting off, suspending the viewer,
removing a report, or reducing its binding closes team access.

Snapshots refresh every two seconds while this page is visible. All viewers
share a capture cache; captures are at most once every five seconds. A busy
employee is never queued behind a viewer. A paused browser stays paused. The
page labels an older cached image, and clears images when a new request is
refused. Safety holds fully mask an earlier clean image too.

Masking covers password/payment fields and sensitive windows. It does not
remove every name, email address, or other personal detail in a page. Viewing
is recorded without storing the image in the oversight audit. The record
names the actual report whose session was viewed, rather than everyone sharing
that employee. Repeated polls by the same viewer/session are deduplicated for
one minute. Viewing requests are bounded per person; the stop action is not
blocked by the viewing budget.

The team-view stop confirmation is tied to the session being shown. If that session has
ended and a new one has started, the old confirmation cannot stop the new one.

The macOS local-browser target is a separate feature with a different privacy
boundary. Team viewing must not expose its page. Platform and provider
acceptance remain separate from controlled Docker/dashboard fixtures.

## Open a link from a conversation or task

When a container browser starts during a verified conversation, WebChat can
show a viewing button. Telegram, LINE, Discord and Slack use the original
account and thread; a link needs a mapped dashboard user and a configured non-loopback
dashboard address (the administrator must ensure it is reachable). Otherwise the notification is text only. Delivery is best
effort and does not change whether the browser started. Background runs keep
the entry in the matching task or run record instead of impersonating a
conversation.

Sign in normally after opening the link, including any required second factor.
The dashboard returns to that browser. The link expires within 30 minutes,
is tied to one session, and grants no permission. Access is checked again
when opening and viewing it. A forwarded link cannot grant access.

Link-based viewing uses masked snapshots and a stop confirmation for that
exact session, including for directly bound viewers. An expired link or a
replacement session clears the image and controls. Use the page's explicit
current-session button to leave the old link and check the current session
under your usual permissions. Do not use a viewing link as a login credential.
