# SSHSIG interoperability fixtures

`ed25519-{1,2}.pub`, `ed25519-{1,2}-{root,release,wrong}.sig` and
`message.json` were generated on 2026-11-05 with the system
`/usr/bin/ssh-keygen`. Both temporary private keys were removed with their
temporary directory; no private key is included here.

Commands used for each key and namespace:

```sh
ssh-keygen -q -t ed25519 -N '' -C iso-f-public-test-fixture -f "$TEMP/key"
ssh-keygen -Y sign -f "$TEMP/key" -n duduclaw-root-metadata-v1@dudustudio -O hashalg=sha512 "$TEMP/message"
```

The production verifier is stricter than generic OpenSSH: SHA-512 only, empty
reserved field, exact version/namespace, canonical base64, no trailing data.
The unit tests mutate the independently generated envelopes and reject each
truncated byte sequence. Synthetic Ed25519 security-key signatures exercise the
authenticator hash, application, UP/UV and counter parsing; they are **not** a
real hardware interoperability result.

## Required real FIDO2 acceptance

The ignored test `real_fido2_sk_ed25519_openssh_vector` fails until real fixtures
exist. On macOS use Homebrew OpenSSH (`/opt/homebrew/bin/ssh-keygen`), because
Apple's bundled build may not provide the security-key backend. Connect a FIDO2
key that supports Ed25519, and use a private directory **outside this repository**:

```sh
/opt/homebrew/bin/ssh-keygen -t ed25519-sk -O verify-required -O application=ssh:duduclaw-target -f "$PRIVATE_DIR/target"
mkdir -p crates/duduclaw-updated/tests/fixtures/fido2
cp crates/duduclaw-updated/tests/fixtures/message.json "$PRIVATE_DIR/message.json"
/opt/homebrew/bin/ssh-keygen -Y sign -f "$PRIVATE_DIR/target" -n duduclaw-release-manifest-v1@dudustudio -O hashalg=sha512 "$PRIVATE_DIR/message.json"
cp "$PRIVATE_DIR/target.pub" crates/duduclaw-updated/tests/fixtures/fido2/target.pub
cp "$PRIVATE_DIR/message.json" crates/duduclaw-updated/tests/fixtures/fido2/message.json
cp "$PRIVATE_DIR/message.json.sig" crates/duduclaw-updated/tests/fixtures/fido2/message.sig
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated real_fido2_sk_ed25519_openssh_vector -- --ignored
```

Do not copy the private key handle. Verify PIN and touch are both required.
`sk-ecdsa-sha2-nistp256@openssh.com` remains unsupported and fails closed; a
device lacking Ed25519 support cannot sign accepted releases in this package.

## Release tooling

`scripts/sign-isolation-manifest.py` reads a contract-shaped JSON template,
recomputes every artifact size and SHA-256 from `--artifacts-dir/<artifact-id>`,
writes compact sorted-key UTF-8 JSON, and signs it with the fixed role namespace.
It emits `<output>.signatures.json` containing the detached armor strings.
Root/emergency metadata use `--kind root` / `--kind emergency` and repeat `--key`
for distinct signers. No private keys belong in this repository.

The script does not prove threshold authorization or accept the release on a
client: run the Rust verifier and the installation tests before publication.
