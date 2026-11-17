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

## Test keys

Implementation and VM acceptance use the TEST key set of
`scripts/isolation-test-keys.sh` (owner decision Q1). Its pin only ever goes into a
`--cfg duduclaw_test_root` build, which prints `TEST <pin>` and is refused by every
release path; it is never written to this directory.
