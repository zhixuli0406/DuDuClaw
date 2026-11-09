# Linux protected mode: acceptance runbook (v1.81)

[繁體中文](zh-TW/linux-protected-acceptance.md) · [日本語](ja-JP/linux-protected-acceptance.md)

This runbook is for release maintainers. It lists what you need, which commands to run, and how to read the results for the native Linux protected-mode acceptance of v1.81: Personal and Shared employees, connectors, and the Package H tools.

> **Every step below is destructive.** The harnesses install the protected service, create system accounts, write under `/var/lib/duduclaw*`, `/opt`, `/etc`, change `/etc/hosts`, install a throw-away CA and remove all of it afterwards. Use a disposable VM. They refuse to run unless `DUDUCLAW_ISO_DISPOSABLE=1` is set, and refuse when a DuDuClaw installation exists that they did not create. They never use your own `~/.duduclaw` or your own Claude / agy login: `HOME` and `DUDUCLAW_HOME` of the operator CLI point into a private temp directory.

## What is covered

| Part | Tool | Covers |
|---|---|---|
| G1 | `cargo test -p duduclaw-cli` (unit test, any Linux builder, no root) | Every catalog tool (263) through the real issuer and the real connector factory for one Personal and one Shared employee: route, required grants, eligibility per employee kind, no missing factory |
| G3 | `scripts/live-test/linux-protected/connectors-acceptance.sh` | Matrix 1–15: Personal bindings, revocation, restart, setup-required, provider smoke tests |
| G2 | `iso_personal_chain` test binary (root) | Personal: request → approval → allocation → run with the owner's connector → tombstone (stops the live run) → restore (same uid) → adoption after a spawnd state reset (new uid, old token refused) → purge + retire (label free, new id and uid). Shared: tombstone → restore |
| H15 | `scripts/live-test/linux-protected/package-h-acceptance.sh` + `iso_package_h_chain` test binary (root) | Matrix 16–27: host facts, brokered file tools, public-web tools and their refusals, in-run helpers (timeout ≥ 305 s projection, cancel), fixture skill hub and hostile archives, spawn tools under D8, the recording gate, readiness reasons |

## Prerequisites

**Hosts.** One disposable VM per distribution: Ubuntu 22.04, Ubuntu 24.04, Debian 12. Root access, systemd as PID 1, unified cgroup v2, not a container. Packages: `python3` (≥ 3.10), `util-linux` (`setpriv`), `passwd`, `openssl`, `ca-certificates`, `coreutils`, `procps`. About 2 GB free under `/var` and `/opt`.

**Builds.** A candidate `duduclaw` binary that you verified yourself, and the two chain test binaries built from the same commit:

```sh
cargo test -p duduclaw-gateway --test iso_package_h_chain --no-run   # prints .../deps/iso_package_h_chain-<hash>
cargo test -p duduclaw-gateway --test iso_personal_chain --no-run    # prints .../deps/iso_personal_chain-<hash>
mkdir -p /abs/tests && cp <printed paths> /abs/tests/   # then rename to iso_package_h_chain / iso_personal_chain
```

**Tool driver.** No dashboard RPC runs an employee tool, so every live tool call goes through an executable you provide (`DUDUCLAW_ISO_TOOL_DRIVER`). Its contract is in `scripts/live-test/linux-protected/README.md` ("Tool driver contract"); H15 adds `seed` and `cancel`. Without it those checks are `SKIP`.

**Signed helper runtimes** (H21/H22): the release publication must carry the program / code-map / office helper runtimes; otherwise spawnd does not advertise `helper_calls` and the helper checks refuse by design.

**Fixture origin and hub** (H19/H20, H23/H24): a **global** IP address on the host (`--fixture-ip`). Protected web access only reaches public addresses, so a private-LAN address (e.g. 192.168.x.x) is refused by design and those checks `SKIP`. The harness serves `fixtures/public_origin.py` on that address with a throw-away CA and `/etc/hosts` entries for `h15.fixture-duduclaw.net`, `rebind.fixture-duduclaw.net` (→ 127.0.0.1) and `clawhub.ai`. This only works if the product's DNS and TLS stack honour `/etc/hosts` and the system trust store; if they do not, H19/H23 fail with a DNS/TLS error and you need a real public fixture host instead (`DUDUCLAW_ISO_PUBLIC_ORIGIN`).

**Second Admin** (G2 restore, purge, retire): the restore and early-purge approval rule needs an Admin who has been eligible for at least 7 days, or a host whose admin log is that old. On a fresh host these steps are `SKIP` with the reason. To cover them, keep a long-lived disposable host and put the second Admin's session state in `DUDUCLAW_ISO_STATE_DIR/admin2.json`.

**Provider test accounts the owner must supply** (G3 items 1, 3, 7–9, 14, 15; none are needed for G1, G2 or H15):

| Provider | What | Variable |
|---|---|---|
| Google | Workspace test tenant: two users with personal OAuth (Personal), Home OAuth, delegate / service account | `DUDUCLAW_ISO_GOOGLE_ALICE=1`, `DUDUCLAW_ISO_GOOGLE_HOME=1` |
| Notion | Workspace credential | `DUDUCLAW_ISO_NOTION=1` |
| GitHub | Test org / repo credential | `DUDUCLAW_ISO_GITHUB=1` |
| LINE | Bot channel token (also Discord / Telegram) | `DUDUCLAW_ISO_LINE_TOKEN` (`_DISCORD_TOKEN`, `_TELEGRAM_TOKEN`) |
| Odoo | Disposable instance + API key | `DUDUCLAW_ISO_ODOO_URL` (+ `_DB`, `_USER`, `_KEY`) |
| Database source | Read-only Postgres / MySQL user and one table | `DUDUCLAW_ISO_PG_DSN_RO` + `_PG_TABLE`, `DUDUCLAW_ISO_MYSQL_DSN_RO` + `_MYSQL_TABLE` |
| Search | Search provider key | `DUDUCLAW_ISO_SEARCH_API_KEY` |

Connect the OAuth accounts with `connectors-acceptance.sh --pause-after-setup`. Never put real tokens in shared shell history: pass them with `sudo env VAR=… script`.

## Commands

1. **G1** (Linux builder, not root):

   ```sh
   cargo test -p duduclaw-cli g1_real_issuer_and_factory -- --nocapture
   ```

2. **G3** (each distribution):

   ```sh
   sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
     bash scripts/live-test/linux-protected/connectors-acceptance.sh \
     --candidate /abs/candidate-dir --out /abs/evidence/g3-ubuntu2204
   ```

3. **H15 + G2** (each distribution; one script runs both chains):

   ```sh
   sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
     bash scripts/live-test/linux-protected/package-h-acceptance.sh \
     --candidate /abs/candidate-dir --tests-dir /abs/tests --out /abs/evidence/h15-ubuntu2204 \
     [--fixture-ip <global address of this host>] [--allow-spawnd-reset]
   ```

   `--allow-spawnd-reset` lets G2 move `/var/lib/duduclaw-spawnd` aside to prove adoption; every employee on the host loses its root allocation, so run it only where nothing else matters. `--only h15` or `--only g2` runs one chain. `--keep` keeps the install for investigation; `--reset` removes a previous run.

4. **spawnd and helper root tests** (unchanged, each distribution): `DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1`, plus the recording root tests in `crates/duduclaw-execution/tests/recording_root.rs` (`ISO_ROOT_CHROMIUM`).

## Reading the results

- Every check prints one line: `PASS`, `FAIL` or `SKIP`, an id and a detail. Ids from the chain binaries are prefixed `H15:` or `G2:` (for example `G2:G2-05a`). The script ends with a table and the counts.
- Exit status `1` means at least one `FAIL` or an interrupted run. Exit `0` can still contain `SKIP` lines: **read them**. A `SKIP` always names what was missing (driver, fixture address, second Admin, permission to reset spawnd). It never counts as a pass.
- A `FAIL` that says "… is not implemented in this build (F6c / F6g / F6l / F6 §3.8 / F6 §3.9)" means the RPC for restore, adoption, purge or retire is not in the candidate yet. That is a real gap, not a host problem.
- `OUT/facts.tsv` records the host facts per run: distribution, kernel, systemd, Landlock ABI, LSMs, spawnd feature bits (`work_broker`, `helper_calls`, `helper_recording`) and whether a recording probe ran. Keep it with the evidence for each of the three distributions. Ubuntu 22.04 and Debian 12 have Landlock ABI < 4 (no TCP port pinning; documented residual).
- `OUT/<binary>.log` is the full test output; `OUT/evidence/` holds RPC replies (no tokens), driver replies and fixture hit logs; `journal.log` holds all `duduclaw*` units.
- The final `PASS cleanup` line means the install, units, accounts, fixture CA and hosts entries are gone. `FAIL cleanup` keeps the staging directory for a later `--reset`.

**Release decision.** Ship only when G1 passes on the Linux builder, and the † items of G3 plus every H15 and G2 check either pass or have a documented, accepted `SKIP` on all three distributions. Expected `SKIP`s on a fresh host: G2 restore / purge / retire (approval rule), and H26 when no recording runtime is published. The recording gate must stay closed (`H16-recording` PASS) until the recording probe has passed on every distribution.
