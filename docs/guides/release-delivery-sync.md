# Release documentation and website synchronization

Every new platform release requires a feature impact review of two complete
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

Obtain the neutral `vNEXT.json` assertion and `vNEXT.sig` from the private
release-delivery procedure. Assertions expire within 30 days and bind the exact
pre-bump source and binary Git diff since the preceding release tag. Any new
source commit requires another content impact review and signature.

```bash
export RELEASE_DELIVERY_ASSERTION=/absolute/path/to/vNEXT.json
export RELEASE_DELIVERY_SIGNATURE=/absolute/path/to/vNEXT.sig
./scripts/release.sh patch --title "Reviewed release theme"
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

## Merge, then publish

Push the release branch, open its PR, run required checks and obtain the owner's
merge approval. Local tags prepared by the script are not permission to publish.
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
Deploy reviewed website builds through each site's existing deployment procedure
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
