# TODO — Bootstrap admin cannot reach the dashboard on a containerised install

**Owner:** — &nbsp; **Status:** confirmed, not started
**Last updated:** 2026-08-16 &nbsp; **Severity:** 🟠 HIGH (blocks first login on a whole deployment class)

## Problem

On a fresh install reached through Docker port forwarding, the operator can log
in successfully and still never get past a loading spinner. The dashboard is
unusable and nothing on screen says why.

Reproduced 2026-08-16 on a new enterprise container (host `18999` → container
`18789`, v1.61.0). Console shows `POST /api/session/local 403` followed by an
endless `ws://…/ws` retry loop.

## Root cause

Three behaviours that are each individually correct combine into a deadlock:

1. **The bootstrap admin is created with `must_change_password = 1`.**
   `duduclaw_auth::db` generates `admin@local` plus a one-time random password
   on first boot when the user table is empty, and flags it.

2. **`authenticate_jwt` refuses the WebSocket handshake while that flag is
   set.** The server replies
   `JWT authentication failed: password change required before any operation`
   and closes. Confirmed by hand-rolling the handshake — `/api/login` still
   issues a token and `/api/me` still returns 200, so the failure is invisible
   to every check the frontend makes before opening the socket.

3. **The passwordless escape hatch cannot apply here.**
   `POST /api/session/local` performs an implicit claim that clears the flag,
   but `local_session::evaluate` requires *both* the Personal edition **and** a
   loopback source address. An enterprise container behind a Docker port
   forward fails both: the edition is Enterprise, and the source IP the gateway
   sees is the bridge network (`172.x`), not loopback.

The frontend then lands in `AuthGuard` case 2 — authenticated, WS not
authenticated — which renders `ConnectingSpinner` indefinitely. The one UI that
could clear the flag (account settings) lives behind that same gate.

```
login OK ──> isAuthenticated = true ──> AuthGuard waits for WS
                                              │
                            WS handshake ─────┴──> refused: must_change_password
                                              │
                                        retry forever ──> spinner
```

## Why it has not been seen before

The Personal edition on loopback takes the `/api/session/local` path, which
claims the account silently. Existing container deployments were first set up
through an in-container loopback session, which cleared the flag before anyone
opened a browser. Only a *fresh* container whose first contact is an external
browser hits it.

## Current workaround

`POST /api/change-password` deliberately bypasses `authenticate_jwt` precisely
so a flagged user can recover, and it works — but nothing in the UI or the logs
points the operator at it:

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:<port>/api/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"admin@local","password":"<one-time password from boot log>"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')

curl -s -X POST http://127.0.0.1:<port>/api/change-password \
  -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  -d '{"new_password":"<a strong password>"}'
```

## Proposed fix

Two changes, either of which breaks the deadlock; doing both is better.

**A. Frontend — react to the specific error instead of retrying.**
The WS handshake already returns a distinguishable message. Surface it: when
the connect response carries `password change required`, route to a
change-password screen rather than looping. Cheap, no protocol change, and it
turns a dead spinner into an actionable prompt.

**B. Backend — allow a restricted authenticated state.**
Let the handshake succeed for a `must_change_password` user but hand back a
`UserContext` that permits only the change-password call, mirroring how
`/api/change-password` already trusts a freshly issued access token. This makes
the in-app flow work without special-casing the client.

Worth considering alongside: have the gateway log the recovery command next to
the generated password, so the boot log is self-sufficient.

## Acceptance

- A brand-new container, first contact from an external browser, reaches a
  usable dashboard using only what the boot log prints.
- No regression for the Personal + loopback passwordless path.
- A user with `must_change_password` set still cannot perform any operation
  other than changing the password.

## Evidence

Server response captured from a hand-rolled handshake against the affected
container:

```json
{"type":"res","id":"1","ok":false,
 "error":"JWT authentication failed: password change required before any operation"}
```

After `POST /api/change-password`, the same handshake returns:

```json
{"type":"res","id":"1","ok":true,
 "payload":{"status":"authenticated","user":{"email":"admin@local","role":"admin"}}}
```

## Related code

- `crates/duduclaw-gateway/src/server.rs` — `handle_local_session`,
  `handle_change_password`, WS `connect` handling and `authenticate_jwt`
- `crates/duduclaw-gateway/src/local_session.rs` — the six-condition gate
- `crates/duduclaw-auth/src/db.rs` — bootstrap admin creation
- `web/src/components/AuthGuard.tsx` — the spinner branch
- `web/src/stores/auth-store.ts` — `tryLocalSession`, `loadFromStorage`
