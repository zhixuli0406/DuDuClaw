#!/usr/bin/env bash
# TEST-ONLY isolation key kit (plan §3.3, R2 §4.2; owner decision Q1: implementation
# and VM acceptance use test root keys, the real ceremony happens before release).
#
#   scripts/isolation-test-keys.sh keys   [--out <new dir>] [--signer <duduclaw-isolation-test-signer>]
#   scripts/isolation-test-keys.sh bundle --keys <keys dir> --archive <duduclaw-linux-<arch>.tar.gz>
#                                         --target <triple> [--out <new dir>] [--signer <path>]
#   scripts/isolation-test-keys.sh --help
#
# `keys` creates the TEST root key set with the test signer (R1 simulated
# sk-ssh-ed25519 "ssh:duduclaw-root", R2/R3 ssh-ed25519, target simulated
# sk-ssh-ed25519 "ssh:duduclaw-target", plus a TEST minisign key standing in for
# the legacy release key), builds root.json with scripts/isolation-root-metadata.py,
# signs it with R1+R2 through scripts/sign-isolation-manifest.py, and prints
# `TEST <pin>` with the exact environment for a TEST verifier build.
#
# `bundle` turns an archive made by scripts/package-linux-release.py WITHOUT a
# production pin (e.g. --skip-build after a normal build) into the four migration
# files of plan §3.2, using the production code paths: --from-release-archive
# (target signs the manifest), --migration-bundle-from (ustar), sign-isolation-manifest
# --kind bundle (R1+R2 BundleAuthorization) and a TEST minisign signature.
#
# These keys are worthless outside a `--cfg duduclaw_test_root` verifier, which
# prints `TEST <pin>`; release.yml, release.sh and package-linux-release.py all
# require PRODUCTION. Output defaults to a new directory under $TMPDIR and is
# refused inside this repository. Private TEST keys are 0600 in a 0700 directory;
# delete the directory when the VM run is over.
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
SIGNER=""
OUT=""
# Output directory of this run; removed again if the run fails part-way, so a
# half-made TEST key set never lingers. Cleared once the run has succeeded.
CLEANUP_DIR=""
cleanup() {
    local status=$?
    if [[ "$status" -ne 0 && -n "$CLEANUP_DIR" ]]; then rm -rf -- "$CLEANUP_DIR"; fi
}
trap cleanup EXIT

usage() {
    # The comment block at the top of this file, without the leading "# ".
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
}

fail() {
    echo "isolation-test-keys: $*" >&2
    exit 1
}

# A new 0700 directory outside the repository (default: under $TMPDIR).
make_out_dir() {
    local requested="$1" parent resolved
    if [[ -z "$requested" ]]; then
        mktemp -d "${TMPDIR:-/tmp}/duduclaw-isolation-TEST-keys.XXXXXX"
        return
    fi
    if [[ -e "$requested" || -L "$requested" ]]; then fail "--out must not exist yet"; fi
    parent="$(cd -- "$(dirname -- "$requested")" && pwd -P)" || fail "--out parent does not exist"
    resolved="$parent/$(basename -- "$requested")"
    case "$resolved/" in
        "$ROOT"/*) fail "--out must be outside the repository (TEST keys never enter the workspace)" ;;
    esac
    mkdir -m 0700 -- "$resolved"
    printf '%s\n' "$resolved"
}

ensure_signer() {
    if [[ -n "$SIGNER" ]]; then
        [[ -x "$SIGNER" && -f "$SIGNER" ]] || fail "--signer must be an executable file"
        return
    fi
    # Separate target directory: the feature build never mixes with release outputs.
    (cd -- "$ROOT" && CARGO_TARGET_DIR="$ROOT/target/isolation-test-signer" \
        cargo build --locked -p duduclaw-updated --features test-signer --bin duduclaw-isolation-test-signer)
    SIGNER="$ROOT/target/isolation-test-signer/debug/duduclaw-isolation-test-signer"
}

file_sha256() {
    python3 -c 'import hashlib, sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as source:
    for chunk in iter(lambda: source.read(1 << 20), b""):
        digest.update(chunk)
print(digest.hexdigest())' "$1"
}

cmd_keys() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --out) OUT="${2:?--out needs a value}"; shift 2 ;;
            --signer) SIGNER="${2:?--signer needs a value}"; shift 2 ;;
            *) fail "unknown keys option '$1'" ;;
        esac
    done
    ensure_signer
    local out expires pin minisign_public
    out="$(make_out_dir "$OUT")"
    CLEANUP_DIR="$out"
    chmod 0700 -- "$out"
    "$SIGNER" keygen root-sk "$out/root-r1" >/dev/null
    "$SIGNER" keygen root "$out/root-r2" >/dev/null
    "$SIGNER" keygen root "$out/root-r3" >/dev/null
    "$SIGNER" keygen target-sk "$out/target" >/dev/null
    "$SIGNER" keygen minisign "$out/minisign" >/dev/null
    expires=$(( $(date +%s) + 365 * 86400 ))
    python3 "$ROOT/scripts/isolation-root-metadata.py" --root-key "$out/root-r1.pub" --root-key "$out/root-r2.pub" \
        --root-key "$out/root-r3.pub" --target-key "$out/target.pub" --expires-at-unix "$expires" \
        --output "$out/root.unsigned.json" >/dev/null
    python3 "$ROOT/scripts/sign-isolation-manifest.py" --kind root --template "$out/root.unsigned.json" \
        --output "$out/root.json" --key "$out/root-r1" --key "$out/root-r2" --test-signer "$SIGNER" >/dev/null
    pin="$(file_sha256 "$out/root.json")"
    minisign_public="$("$SIGNER" minisign-public "$out/minisign")"
    printf '%s\n' "$pin" > "$out/TEST-root.sha256"
    CLEANUP_DIR=""
    cat <<EOF
TEST $pin
TEST key set: $out
Build a TEST verifier (never a release build; it prints TEST $pin):
  RUSTFLAGS='--cfg duduclaw_test_root' DUDUCLAW_BOOTSTRAP_ROOT_SHA256=$pin \\
  DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY=$minisign_public \\
  cargo build --release -p duduclaw-updated --bin duduclaw-bootstrap
Then: $0 bundle --keys $out --archive <duduclaw-linux-<arch>.tar.gz> --target <triple>
EOF
}

cmd_bundle() {
    local keys="" archive="" target=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --keys) keys="${2:?--keys needs a value}"; shift 2 ;;
            --archive) archive="${2:?--archive needs a value}"; shift 2 ;;
            --target) target="${2:?--target needs a value}"; shift 2 ;;
            --out) OUT="${2:?--out needs a value}"; shift 2 ;;
            --signer) SIGNER="${2:?--signer needs a value}"; shift 2 ;;
            *) fail "unknown bundle option '$1'" ;;
        esac
    done
    [[ -n "$keys" && -n "$archive" && -n "$target" ]] || fail "bundle needs --keys, --archive and --target"
    [[ -f "$keys/root.json" && -f "$keys/TEST-root.sha256" ]] || fail "--keys is not a key set made by '$0 keys'"
    local arch
    case "$target" in
        x86_64-unknown-linux-gnu) arch="x86_64" ;;
        aarch64-unknown-linux-gnu) arch="aarch64" ;;
        *) fail "unknown --target" ;;
    esac
    ensure_signer
    local out version tar_name expires
    out="$(make_out_dir "$OUT")"
    CLEANUP_DIR="$out"
    chmod 0700 -- "$out"
    version="$(python3 -c 'import importlib.util, sys
spec = importlib.util.spec_from_file_location("packager", sys.argv[1])
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)
print(packager.version_text(packager.workspace_version()))' "$ROOT/scripts/package-linux-release.py")"
    python3 "$ROOT/scripts/package-linux-release.py" --target "$target" --from-release-archive "$archive" \
        --manifest-template "$ROOT/release/isolation/manifest-template.json" --manifest-valid-days 30 \
        --signing-key "$keys/target" --test-signer "$SIGNER" \
        --bootstrap-root "$keys/root.json" --bootstrap-root-signatures "$keys/root.json.signatures.json" \
        --output-dir "$out"
    python3 "$ROOT/scripts/package-linux-release.py" --target "$target" \
        --migration-bundle-from "$out/isolated-linux-$arch-bootstrap-inbox" --output-dir "$out"
    tar_name="duduclaw-isolated-linux-$arch-$version.tar"
    expires=$(( $(date +%s) + 30 * 86400 ))
    python3 "$ROOT/scripts/sign-isolation-manifest.py" --kind bundle --bundle "$out/$tar_name" \
        --expires-at-unix "$expires" --output "$out/$tar_name.authorization.json" \
        --key "$keys/root-r1" --key "$keys/root-r2" --test-signer "$SIGNER" >/dev/null
    "$SIGNER" minisign "$keys/minisign" "$out/$tar_name" "duduclaw v$version $tar_name" "$out/$tar_name.minisig"
    CLEANUP_DIR=""
    echo "TEST $(cat -- "$keys/TEST-root.sha256")"
    echo "TEST-ONLY migration bundle in $out (copy these four into /root/duduclaw-migrate on the VM):"
    local file
    for file in "$tar_name" "$tar_name.minisig" "$tar_name.authorization.json" "$tar_name.authorization.json.signatures.json"; do
        printf '  %s  %s\n' "$(file_sha256 "$out/$file")" "$file"
    done
}

case "${1:-}" in
    keys) shift; cmd_keys "$@" ;;
    bundle) shift; cmd_bundle "$@" ;;
    -h|--help) usage ;;
    *) usage >&2; exit 2 ;;
esac
