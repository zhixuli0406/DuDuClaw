# Native Linux isolation release inputs

Public, reviewed inputs of the offline isolation release (plan R2 §4.2). Nothing in
this directory is secret, and no key, key handle, passphrase or PIN may ever be
added here.

## `manifest-template.json`

Base template of the signed release manifest (`ReleaseManifest` in
`crates/duduclaw-isolation/src/update.rs`). It deliberately has no `version`,
`arch`, `serial`, `created_at_unix` or `expires_at_unix`: the signing ceremony
(`scripts/isolation-sign`, see below) renders those per release and architecture through
`scripts/package-linux-release.py --from-release-archive --manifest-valid-days N`
(serial = `major*10^6 + minor*10^3 + patch`, strictly increasing with the
version). Artifact ids are the fixed names the update service downloads
(`isolated-linux-<arch>-<id>`, `crates/duduclaw-updated/src/publication.rs`);
`sha256` and `size_bytes` are bound from the released bytes when the target key
signs. Changing the component set, a protocol version or `minimum_version` is a
reviewed change of this file.

## Signed isolation policy (`isolation-policy-v1` artifact)

The reviewed policy document bound by the manifest is checked offline by
`validate_policy_document` in `scripts/sign-isolation-manifest.py` and again by
spawnd. Policy v4 (domain `duduclaw-isolation-policy-v4`) is v3 plus the
required `dns_probe_name`: choose, at signing time, a hostname AI employees
need anyway (for example the model API host) — lower-case ASCII, at least two
labels, no trailing dot, not an IP address, not `localhost` or a
`.local`/`.internal`/`.localhost` name. spawnd asks egress to resolve it through
the configured DNS route as its DNS readiness evidence, and never reports the
DNS route as verified with a policy older than v4.

## `../bootstrap-root.sha256` (not present yet)

The production root pin: the SHA-256 of the signed production `root.json`, one
line of 64 lowercase hex digits. It is added by a reviewed PR only after the key
ceremony. While it is absent there is no production pin, and therefore:

- `release.yml` builds and ships the Linux archives **without** `duduclaw-bootstrap`
  (the job summary says so for each architecture);
- the signing ceremony (`scripts/isolation-sign`) refuses to run.

When it is present, CI builds `duduclaw-bootstrap` with exactly this pin, executes
it on a host of each target architecture (aarch64 on a native arm64 runner) and
requires `PRODUCTION <pin>` before packaging it.

## Starting the signing ceremony

The offline ceremony is started ONLY as

    scripts/isolation-sign --root-metadata … --root-signatures … --output-dir …

from the repository top level, in a fresh terminal, as yourself (see
`scripts/isolation-sign --help`). Never `bash scripts/release.sh isolation-sign`,
never `bash scripts/isolation-sign`, never through `sudo`. The launcher's first
line, `#!/usr/bin/env -S -i /bin/bash --noprofile --norc`, makes the kernel start
it with an empty environment, so no exported shell function, `BASH_ENV` or other
variable of the calling shell reaches the ceremony; `scripts/release.sh
isolation-sign` refuses to run unless that launcher started it. The shell you
type the command into is still trusted: it chooses which file is started.

`--output-dir` is not where the ceremony works. Every asset is produced, signed,
verified, hashed and uploaded from a private staging directory inside the
ceremony's own 0700 work directory (removed on every exit; a failure deletes only
there), and everything is verified again on those staged bytes immediately before
the upload. Only at the very end — after the upload and its read-back, or with
`--no-upload` after that final verification — the results and
`SHA256SUMS-isolation.txt` are copied into `--output-dir` as a convenience. It must
not exist yet, and its parent must be an existing directory owned by you that is
neither a symbolic link nor group/other-writable (checked before any prompt and
again right before the copy). It is created 0700, each file 0600, and each copy is
checked against the staged SHA-256. A failed copy never affects what was uploaded.

## Test keys

Implementation and VM acceptance use the TEST key set of
`scripts/isolation-test-keys.sh` (owner decision Q1). Its pin only ever goes into a
`--cfg duduclaw_test_root` build, which prints `TEST <pin>` and is refused by every
release path; it is never written to this directory.

The same kit makes the TEST licence the VM install needs (owner decision
2026-11-18: no production team licence is issued for tests). Only a test-root
`duduclaw` compiled with `DUDUCLAW_TEST_LICENSE_ISSUER_PUBKEY` trusts its issuer
(key id `test-root`; `duduclaw print-license-issuer` prints `TEST-LICENSE <key>`,
a release build `PRODUCTION-LICENSE <16 hex>`); a production build contains no
trace of it. Release tooling refuses both: `scripts/package-linux-release.py`
refuses test-root RUSTFLAGS and that variable on every build or packaging run and
statically refuses a test-licence `duduclaw` (it carries a fixed marker) in
packaging, the cross-build hand-off, `--verify-archive` and
`--from-release-archive` (so also `scripts/release.sh isolation-sign`); the only
exception is `--test-signer`, whose outputs are TEST-ONLY.

Every other release path refuses a test-licence build the same way, through one
script, `scripts/refuse-test-license-build.py` (it reads the marker from its single
definition in `crates/duduclaw-gateway/src/license_runtime.rs` at run time, so no
copy of it exists anywhere else): `release.yml` (job-start environment check, the
macOS/Windows gateway before signing and packaging, every archive before minisign
and the GitHub release), `desktop-release.yml` (the staged sidecar before
tauri-action), `container/Dockerfile.server` (the build stage greps the built
binary for the same marker before the runtime image copies it), `npm/scripts/publish.sh`
(every binary before `npm publish`), `scripts/release.sh` (environment check before
any change; the Pro binary before signing and upload) and the Linux packager, which
shares its environment rules. For source-derived markers, the script and Docker
guard print a notice and skip scanning only when `license_runtime.rs` has no marker
definition and contains no `duduclaw_test_root` string (source predating the test-licence
issuer). A missing file, multiple definitions, or test-root source without a definition
still fails closed; `--marker-file` remains strict. `scripts/isolation-test-keys.sh bundle --gateway`
names its TEST copy `duduclaw-linux-<arch>-TEST.tar.gz`. By hand:
`python3 -I -B scripts/refuse-test-license-build.py <binary or .tar.gz/.tgz/.zip>...`
(exit 0 clean, 1 test-licence build found, 2 usage or read error) and
`python3 -I -B scripts/refuse-test-license-build.py --check-environment` (refuses
test-root RUSTFLAGS, the TEST issuer/key variables, any `RUSTC`/`RUSTC_WRAPPER`/
`RUSTC_WORKSPACE_WRAPPER`, and `.cargo/config*` naming the cfg). Cargo configuration
files with a top-level `include` (never followed), decoded strings naming the cfg,
release-sensitive `[env]` names, `build.rustc-wrapper`, `build.rustc-workspace-wrapper`
or `build.rustc`, or that cannot be read or safely parsed, are also refused.
This check prevents accidental test-licence shipments through stray or unknown release-machine
configuration; control of the machine or toolchain is covered by the signed-source and review
process, since an attacker could change the compiler, this script or the sources.
