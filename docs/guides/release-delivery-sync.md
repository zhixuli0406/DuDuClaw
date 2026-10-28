# Release documentation and website synchronization

Every minor/major platform release requires a feature impact review of two complete
manuals (IT administration and end users) and three websites (product, OS and
company). This applies to additions, changed defaults/settings, fixes,
deprecations, removals, security restrictions and installation requirements.
The OS retains its own version and release cadence; reviewing its website does
not imply shipping an OS image or promising unverified hardware support.

The actual manuals, feature impact inventory and acceptance evidence belong to
the separate private delivery repository. They are never copied into this public
repository, release assets, Actions artifacts or website upload contexts. The
public gate receives only a signed assertion with version/source/delta digests,
five neutral item names, completion states and evidence digests.

## Release lanes

- `scripts/release.sh minor` and `major` keep the full process below: both
  handbooks and all three websites require review, final artifacts and a signature.
- `scripts/release.sh hotfix` bumps the patch version. `patch` is a deprecated
  alias with the same behavior. The private tool automatically signs five
  `unchanged` items using artifact digests from the preceding public assertion;
  it creates a hotfix reason naming the new version and binds generated commit/path
  evidence and a private automatic review record. No handbook writing, website
  rebuild or distributor technical brief is required. The public verifier and
  all tag delivery gates remain identical.

Before hotfix signing, the private signer checkout must be clean, including
untracked files. `release.sh` fetches private `origin/main` and requires private
HEAD to be its ancestor, so the signer must already be merged privately. The
fetch explicitly updates `refs/remotes/origin/main`; signer Git queries ignore
replace objects and inherited `GIT_*` overrides.

New public assertions use schema 2: the signed `lane` and `review_kind` are
`full` / `independent` or `hotfix` / `automatic-hotfix-retention`. Schema 1 remains
valid as legacy full/independent evidence. Hotfix schema 2 requires all five items
`unchanged` and exactly one patch bump. Code/UI changes outside the documentation
paths remain eligible; those paths are a drift check, not a code allowlist.

Hotfix also requires pre-bump `HEAD:Cargo.toml` workspace version to equal the
previous tag's version, and that tag to be the highest semver `vX.Y.Z` ancestor
of HEAD. It verifies the committed previous assertion and signature with
`RELEASE_DELIVERY_PUBLIC_KEY_PEM`, including schema, source-parent, version and
delta bindings. Historical expiry alone may be ignored. A retired or wrong key
cannot authenticate that history; use the full lane with the current key.

Hotfix refuses a non-patch bump, a missing/invalid previous public assertion, or a delta
from the previous tag to pre-bump HEAD touching `docs/features/`, `docs/guides/`,
`README.md`, `README.zh-TW.md`, `README.ja.md` or `SECURITY.md` (including removals
and renames). Use minor when those changes need updated delivery artifacts. For
reviewed documentation drift that leaves the delivery artifacts valid, pass an
explicit reason; it is stored in the private evidence and bound by its signed digest:

```bash
./scripts/release.sh hotfix --title "Fix summary"
DUDUCLAW_HOTFIX_ALLOW_DOC_DRIFT="Reviewed rationale" ./scripts/release.sh hotfix
```

Both lanes publish all five GitHub binary targets with minisign/cosign, npm, PyPI,
KB bundles, GHCR server and computer-use images, desktop installers, the Pro image,
OEM tar, Pro binaries and control-plane allowlist. Pro images default to
`CLOUD_BUILD=1`; set `CLOUD_BUILD=0` to deliberately build locally. The pushed amd64
image is pulled before tar creation and linux-x64 binary extraction.

## Main platform CI preflight

PR Rust CI runs only on Ubuntu. Main pushes, the daily 18:00 UTC schedule and
manual CI runs cover Ubuntu, macOS and Windows; Windows failures are real failures.
Before any mutation, release preflight reads the remote main tip with `git ls-remote`
and requires pre-bump HEAD to contain it. It queries the latest 100 `ci.yml` runs
on main across all statuses, selects the newest push/schedule ancestor of HEAD,
and blocks while that run is queued, in progress or waiting. A cancelled/skipped
run can be ignored only if a later run exists on a strictly newer main commit;
a manual cancellation with no such successor blocks. Failures never fall back
to older green runs. The selected run must be on remote main's tip or at most
3 commits behind it, and no more than 36 hours old (including runs at the tip).
Both `Rust macos-latest` and `Rust windows-latest` must be present and successful;
failed, cancelled, skipped or missing jobs block minor/major. A non-success run
conclusion blocks even when both platform jobs succeeded. The local `origin/main`
tracking ref is not used as the remote-tip authority.
Missing `gh`, authentication/query failures or absent coverage also block.
Hotfix prints a warning and continues. `DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI="reason"`
explicitly overrides a minor/major block and the final summary records the reason.
Dry-run queries CI and may therefore refuse minor/major, but never signs or changes files.

## Release dependency caches

`release-cache-warm.yml` builds the same five CLI/gateway targets and four desktop
targets in release mode, without tests, packaging or signing. It runs on main
after the listed Rust manifests/lockfiles/toolchain files change, weekly on Sunday
at 18:00 UTC, or by manual dispatch on main. Dispatches on other refs skip every
job. It has only `contents: read` and uses no signing credentials.

Warm and release jobs use the same pinned rust-cache action and shared keys:
`release-${{ matrix.target }}` (workspace `. -> target`) and
`desktop-release-${{ matrix.target }}` (workspaces `. -> target` and
`src-tauri -> target`). The tag jobs set `save-if: false`; only protected main
writes these caches. GitHub's ref scoping prevents tags from restoring PR caches.
PR CI restores main's Rust caches without saving PR-scoped entries; the server
image tag workflow has no GHA cache because tags cannot reuse each other's cache
and main writes no Docker cache scope. The computer-use smoke and push builds
share a run, so they retain a `mode=min` cache to reduce its size.

Keep runner labels, installed Rust toolchains, setup order and compiler environment
identical across each warm/release pair. rust-cache also keys by runner OS/CPU,
rustc identity, environment variables starting with `CARGO`, `CC`, `CFLAGS`, `CXX`,
`CMAKE` or `RUST`, Cargo dependency manifests/locks, `.cargo/config.toml` and
toolchain files. Workspace mappings select the metadata/lockfiles and cached target
paths. Step-local signing variables and `PKG_CONFIG_ALLOW_CROSS` are introduced
after cache restore and do not alter that key. A stable toolchain or runner image
update can cause a cold cache; changed dependency inputs can use a matching
environment's restore prefix. Confirm the computed keys and actual cache hits
in Actions logs before claiming that a particular tag reused a warm cache.

## Prepare the content

1. Create the next release branch from current `origin/main`, after feature PRs
   and their public documentation have merged. Keep the curated `[Unreleased]`
   changelog complete. Record this branch's exact pre-bump source commit and the
   preceding `vX.Y.Z` release tag.
2. In the private repository, inventory the functional changes since that tag.
   For every change, record effects on both manuals and all three websites,
   including configuration names/defaults, privileges and installation paths.
3. Update the complete IT manual (issuance → installation → configuration of
   every current feature) and the complete user manual. Regenerate their final
   PDFs and coverage ledgers, check links and render every PDF page. Update all
   affected website pages/languages, build each website and verify desktop,
   mobile, keyboard and links against those actual built bytes.
4. A maker and a different checker review the exact sources, feature inventory,
   final artifacts and acceptance evidence. An unchanged item still requires a
   concrete reason tied to this feature delta, complete current artifacts and
   acceptance checks. `pending`, skipped, absent or stale evidence blocks the
   release. Filename/version substitutions alone do not count as updating
   content. Commit the private manifest and review before signing.

The private signing tool checks artifact bytes against committed private sources,
and site source/build inventories against all actual files. The owner signs
only after reviewing the content and the independent checker report. The
signature authenticates that approval; software cannot prove semantic
completeness or reviewer independence from a `PASS` field alone.

## Prepare the version bump

For minor/major, obtain the neutral `vNEXT.json` assertion and `vNEXT.sig` from the private
release-delivery procedure. Assertions expire within 30 days and bind the exact
pre-bump source and binary Git diff since the preceding release tag. Any new
source commit requires another content impact review and signature.

```bash
export RELEASE_DELIVERY_ASSERTION=/absolute/path/to/vNEXT.json
export RELEASE_DELIVERY_SIGNATURE=/absolute/path/to/vNEXT.sig
./scripts/release.sh minor --title "Reviewed release theme"
```

The script reads the public verification key from the GitHub repository variable
`RELEASE_DELIVERY_PUBLIC_KEY_PEM`, unless that same public PEM is explicitly set
in the environment. It fails before mutating sources if configuration or delivery
evidence is missing. `audit`, `verify` and `--dry-run` remain read-only; dry-run
does not claim the delivery gate passed. Do not execute bump mode on `main` or a
detached checkout.

After building, the script commits only version metadata, the unchanged curated
feature notes under the new release header, and the neutral signed assertion.
Before retaining local release tags, it validates this actual commit against the
approved source: all platform manifests must be synchronized; extra code changes,
changed file modes, changed feature notes or extra parents are refused. The legacy
technical-brief renderer can still produce its separate private HTML, but that
optional output cannot satisfy either mandatory full-manual item.

## Enterprise channel gate (v1.76.2)

`scripts/release.sh` checks the enterprise prerequisites before it changes anything: the private enterprise checkout, the Pro signing key, `minisign`, `gcloud` and `docker` on `PATH`, a running Docker daemon, an active gcloud account, the token that fetches the distributor notice (read from Secret Manager when `DUDUCLAW_CONTROL_ADMIN_TOKEN` is unset; `DUDUCLAW_DISTRIBUTOR_NOTICE=skip` builds without fetching) and the enterprise checkout on its own branch. A missing one stops the run. A deliberate CE-only release sets `DUDUCLAW_RELEASE_WITHOUT_PRO=1` and is announced as such.

After the tag is created, the Pro image and offline tar, the darwin and linux-x64 Pro binaries, the control-plane allowlist and delivery manifest (checked on the serving revision) must all succeed. The script commits the re-resolved enterprise `Cargo.lock` and (minor/major only) the distributor brief on the enterprise checkout's branch and prints the push command; only a failed commit is a gap. The darwin binary is taken from cargo's actual target directory, so a shared `CARGO_TARGET_DIR` works. Any failure, or any `DUDUCLAW_SKIP_PRO_*` opt-out, is listed at the end and the script exits non-zero without the success banner. `release.sh verify VERSION` also checks that the serving control-plane revision offers the version (`pro-offer`) and fails, instead of staying silent, when it runs without the enterprise checkout.

Added after v1.76.2: the distributor-notice token and enterprise-branch preflight checks, the automatic enterprise commit, and reading the darwin binary from cargo's target directory.

## Merge, then publish

Push the release branch, open its PR and obtain the owner's merge approval.
Minor/major waits for `Rust ubuntu-latest`, `Dashboard`, `Python` and
`Security Audit`. For hotfix fixes and the hotfix bump PR, the owner waits only
for `Rust ubuntu-latest`, then uses `gh pr merge --admin <n> --merge`. Local tags prepared by the script are not permission to publish.
Only push the exact release tags after the bump commit has merged into
`origin/main`. A tag may point to the bump commit itself, or to its normal PR
merge commit when the merge tree exactly equals the reviewed bump tree. If main
advanced and the merged tree contains additional changes, prepare a new review
against that source rather than reusing the old assertion.

```bash
git push origin YOUR_RELEASE_BRANCH
# After the PR checks and owner-approved merge:
git fetch origin main
git merge-base --is-ancestor vNEXT origin/main
git push origin vNEXT desktop-vNEXT
```

Core release, desktop release, server image and computer-use image workflows all
run the same mandatory delivery gate before build/publish. Direct tag pushes and
manual dispatches are covered. The gate requires the tagged commit to be merged
into `origin/main`; every build and registry package checkout uses the verified
commit, including manual dispatch from a different workflow branch. Manual inputs
must be exact version tags; core and desktop tag families are distinct.

After publication, verify registries with `scripts/release.sh verify VERSION`.
For minor/major, deploy reviewed website builds through each site's existing deployment procedure
and verify live pages, headers, assets and browser behavior. Keep private manuals
in the private delivery channel. A build passing does not prove deployment.

## Verification key rotation

The verification PEM is public configuration; the signing key never belongs in
Git, Actions secrets/logs, release assets or a website. Its operator storage and
rotation procedure are documented privately. A release fails closed when the
repository variable is absent, the key is wrong, the assertion expired, or a
signature/digest does not match. Rotating the key requires re-signing any pending
release assertion before its bump commit is prepared. Previously published
binaries are unaffected. Expired assertions and historical tags signed by a
retired key cannot be re-signed in place or republished with this tool: prepare
a new version and complete its delivery review instead.

This workflow cannot prevent an administrator from replacing the key, changing
the workflows or pushing a historical tag whose old workflow predates this
gate. Repository write permissions and owner review remain essential.

Test without issuing a release:

```bash
python3 -m unittest discover -s scripts/tests -p 'test_release_delivery.py' -v
bash -n scripts/release.sh
```
