# Save browser login state

Added in v1.76.0. This feature applies to the container browser on Unix hosts.
It does not use your regular browser profile.

An administrator enables **Allow browser login persistence** under Settings →
System, then **Save this employee's logins** in the employee's permissions.
Both settings are off by default. The equivalent settings are:

```toml
# config.toml
[computer_use.profiles]
enabled = true
max_bytes = 67108864
retention_days = 30 # 0 keeps indefinitely
```

```toml
# agent.toml
[capabilities]
computer_use = true
[capabilities.computer_use_config]
persist_login = true
```

The updated computer-use image must be installed locally; it is not pulled
automatically. Images without the profile helper refuse a persistence start.
The gateway requires its existing machine encryption key. Missing keys never
cause plaintext storage. `duduclaw doctor` reports unavailable configuration.

At a normal session end, Chromium closes before the container exports Cookies,
Local Storage and IndexedDB. The gateway encrypts opaque archive bytes with
AES-256-GCM, bound to this home, employee instance and profile identity. It does
not unpack the archive on the host. Preferences, passwords, extensions, caches
and service workers are not saved. The limit is 64 MiB per employee by default,
at most 128 MiB. A failed save keeps the previous snapshot.
The whole save has a 10-second budget. A new encrypted generation becomes current
only when its index commits. RED, emergency stops and ending an injection-held
session discard current changes. Someone with takeover authority can explicitly
choose to keep logins when ending a held session; RED and emergencies still deny it.

On the next start, the container restores the archive before opening Chromium.
Removing an allowed website resets the entire snapshot; adding websites does
not. A replaced employee, changed owner or primary owner, or owner suspension
cannot reuse the previous owner's snapshot. Removing or archiving an employee
clears saved state. **Clear login state** ends the current browser without
exporting it, then clears the saved snapshot. It requires the same authority as
browser takeover. `duduclaw ops computer-profiles list` only lists metadata.

Saved logins increase what a manipulated browser session can do as that account.
Use a narrow website allowlist and action rules requiring review for clicks and
typing. A person can sign in using takeover; clipboard paste is disabled.
Crash, emergency container removal or a gateway shutdown can lose changes since
the last completed snapshot. Some websites require reauthentication or device
checks. Export and manual/scheduled backups exclude these snapshots.

Validation boundary: archive allowlist/link/path/size tests have run locally.
Final Rust, dashboard, real-container restore, real-site sign-in and cross-platform
verification are pending. Container live-view and local desktop browser work are
separate work packages.
