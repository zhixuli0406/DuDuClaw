#!/usr/bin/env bash
# TEST-ONLY isolation key kit (plan §3.3, R2 §4.2; owner decision Q1: implementation
# and VM acceptance use test root keys, the real ceremony happens before release).
#
#   scripts/isolation-test-keys.sh keys    [--out <new dir>] [--signer <duduclaw-isolation-test-signer>]
#   scripts/isolation-test-keys.sh bundle  --keys <keys dir> --archive <duduclaw-linux-<arch>.tar.gz>
#                                          --target <triple> [--gateway <TEST duduclaw>]
#                                          [--out <new dir>] [--signer <path>]
#   scripts/isolation-test-keys.sh license --keys <keys dir> --fingerprint <value> --out <new file>
#                                          [--tier <tier>] [--days N] [--max-agents N] [--signer <path>]
#   scripts/isolation-test-keys.sh --help
#
# `keys` creates the TEST root key set with the test signer (R1 simulated
# sk-ssh-ed25519 "ssh:duduclaw-root", R2/R3 ssh-ed25519, target simulated
# sk-ssh-ed25519 "ssh:duduclaw-target", plus a TEST minisign key standing in for
# the legacy release key, plus a TEST licence issuer key), builds root.json with
# scripts/isolation-root-metadata.py, signs it with R1+R2 through
# scripts/sign-isolation-manifest.py, and prints `TEST <pin>` with the exact
# environment for a TEST verifier build and a TEST-licence duduclaw build.
#
# `bundle` turns an archive made by scripts/package-linux-release.py WITHOUT a
# production pin (e.g. --skip-build after a normal build) into the four migration
# files of plan §3.2, using the production code paths: --from-release-archive
# (target signs the manifest), --migration-bundle-from (ustar), sign-isolation-manifest
# --kind bundle (R1+R2 BundleAuthorization) and a TEST minisign signature.
# --gateway first replaces duduclaw in a private TEST copy of the archive with the
# TEST-licence build printed by `keys` (the packager never writes such an archive);
# the copy is named duduclaw-linux-<arch>-TEST.tar.gz so it can never be mistaken
# for (or published as) the release archive, and every release path refuses its
# duduclaw (scripts/refuse-test-license-build.py). Without --gateway the bundle
# carries the production gateway, which no TEST licence passes.
#
# `license` signs a TEST license.json (0600) for the installer's licence rule
# (self-host binding, not a Personal edition): --tier self_host_pro (default),
# partner or oem; every other tier is refused by the installer. --fingerprint is
# line 1 of `sudo duduclaw license fingerprint` run ON THE VM (hostname + MAC on
# Linux: read it after the VM has its final hostname). --days defaults to 30
# (at most 366). Copy the file to /root/duduclaw-migrate/license.json (root, 0600).
#
# These keys are worthless outside a `--cfg duduclaw_test_root` verifier, which
# prints `TEST <pin>`, and a test-root duduclaw (`duduclaw print-license-issuer`
# prints `TEST-LICENSE <key>`); release.yml, release.sh and package-linux-release.py
# require PRODUCTION and refuse a test-licence duduclaw. Output defaults to a new
# directory under $TMPDIR and is refused inside this repository. Private TEST keys
# are 0600 in a 0700 directory; delete the directory when the VM run is over.
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
    "$SIGNER" license-keygen "$out/license-issuer" >/dev/null
    expires=$(( $(date +%s) + 365 * 86400 ))
    python3 "$ROOT/scripts/isolation-root-metadata.py" --root-key "$out/root-r1.pub" --root-key "$out/root-r2.pub" \
        --root-key "$out/root-r3.pub" --target-key "$out/target.pub" --expires-at-unix "$expires" \
        --output "$out/root.unsigned.json" >/dev/null
    python3 "$ROOT/scripts/sign-isolation-manifest.py" --kind root --template "$out/root.unsigned.json" \
        --output "$out/root.json" --key "$out/root-r1" --key "$out/root-r2" --test-signer "$SIGNER" >/dev/null
    pin="$(file_sha256 "$out/root.json")"
    minisign_public="$("$SIGNER" minisign-public "$out/minisign")"
    license_public="$(cat -- "$out/license-issuer.pub")"
    [[ "$license_public" =~ ^[0-9a-f]{64}$ ]] || fail "the TEST licence issuer public key is malformed"
    printf '%s\n' "$pin" > "$out/TEST-root.sha256"
    CLEANUP_DIR=""
    cat <<EOF
TEST $pin
TEST key set: $out
Build a TEST verifier (never a release build; it prints TEST $pin):
  RUSTFLAGS='--cfg duduclaw_test_root' DUDUCLAW_BOOTSTRAP_ROOT_SHA256=$pin \\
  DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY=$minisign_public \\
  cargo build --release -p duduclaw-updated --bin duduclaw-bootstrap
Build a TEST-licence duduclaw in its own target directory (never a release build;
\`duduclaw print-license-issuer\` prints TEST-LICENSE $license_public; the packager refuses it):
  CARGO_TARGET_DIR=$ROOT/target/isolation-test-root RUSTFLAGS='--cfg duduclaw_test_root' \\
  DUDUCLAW_TEST_LICENSE_ISSUER_PUBKEY=$license_public \\
  DUDUCLAW_BOOTSTRAP_ROOT_SHA256=$pin DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY=$minisign_public \\
  cargo build --locked --release --target <triple> --no-default-features \\
    --features duduclaw-gateway/dashboard,duduclaw-cli/dashboard,duduclaw-gateway/vendored-openssl \\
    -p duduclaw-cli --bin duduclaw
  (with cross, also CROSS_BUILD_ENV_PASSTHROUGH='RUSTFLAGS DUDUCLAW_TEST_LICENSE_ISSUER_PUBKEY DUDUCLAW_BOOTSTRAP_ROOT_SHA256 DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY')
Then: $0 bundle --keys $out --archive <duduclaw-linux-<arch>.tar.gz> --target <triple> \\
        --gateway $ROOT/target/isolation-test-root/<triple>/release/duduclaw
      $0 license --keys $out --fingerprint <line 1 of 'sudo duduclaw license fingerprint' on the VM> --out <new file>
EOF
}

# A private TEST copy of a release archive (plus its .sha256) whose duduclaw is the
# given TEST-licence build, named <release name>-TEST.tar.gz; prints the copy's path.
# The source archive must match its own .sha256 and hold exactly one regular
# duduclaw; the replacement must carry the test-root marker (parsed by
# scripts/refuse-test-license-build.py from license_runtime.rs).
test_gateway_archive() {
    python3 -I -B -c 'import hashlib, importlib.util, io, os, sys, tarfile
archive, gateway, directory, refuser_path = sys.argv[1:5]
name = os.path.basename(archive)
if not name.endswith(".tar.gz"):
    sys.exit("isolation-test-keys: --archive must be a duduclaw-linux-<arch>.tar.gz release archive")
test_name = name[:-len(".tar.gz")] + "-TEST.tar.gz"
spec = importlib.util.spec_from_file_location("test_license_refuser", refuser_path)
refuser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refuser)
marker = refuser.load_marker()
def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()
with open(gateway, "rb") as source:
    data = source.read()
if marker not in data:
    sys.exit("isolation-test-keys: --gateway is not a TEST-licence duduclaw build (no test issuer marker)")
with open(archive + ".sha256", encoding="ascii") as sidecar:
    if sidecar.read() != f"{sha256(archive)}  {name}\n":
        sys.exit("isolation-test-keys: --archive does not match its .sha256 file")
os.mkdir(directory, 0o700)
copy = os.path.join(directory, test_name)
replaced = 0
with tarfile.open(archive, "r:gz") as source, tarfile.open(copy, "x:gz") as bundle:
    for member in source.getmembers():
        if member.name == "duduclaw":
            if not member.isfile():
                sys.exit("isolation-test-keys: duduclaw in --archive is not a regular file")
            member.size = len(data)
            bundle.addfile(member, io.BytesIO(data))
            replaced += 1
        elif member.isfile():
            bundle.addfile(member, source.extractfile(member))
        else:
            bundle.addfile(member)
if replaced != 1:
    sys.exit("isolation-test-keys: --archive must hold exactly one duduclaw")
with open(copy + ".sha256", "x", encoding="ascii") as sidecar:
    sidecar.write(f"{sha256(copy)}  {test_name}\n")
print(copy)' "$1" "$2" "$3" "$ROOT/scripts/refuse-test-license-build.py"
}

cmd_bundle() {
    local keys="" archive="" target="" gateway=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --keys) keys="${2:?--keys needs a value}"; shift 2 ;;
            --archive) archive="${2:?--archive needs a value}"; shift 2 ;;
            --target) target="${2:?--target needs a value}"; shift 2 ;;
            --gateway) gateway="${2:?--gateway needs a value}"; shift 2 ;;
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
    if [[ -n "$gateway" ]]; then
        [[ -f "$gateway" && ! -L "$gateway" ]] || fail "--gateway must be a regular file"
        archive="$(test_gateway_archive "$archive" "$gateway" "$out/TEST-archive")"
        echo "TEST-ONLY archive with the TEST-licence duduclaw (never upload it): $archive"
    fi
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
        --expires-at-unix "$expires" --output "$out/$tar_name.authorization.json" --root-metadata "$keys/root.json" \
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

cmd_license() {
    local keys="" fingerprint="" output="" tier="self_host_pro" days="30" max_agents=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --keys) keys="${2:?--keys needs a value}"; shift 2 ;;
            --fingerprint) fingerprint="${2:?--fingerprint needs a value}"; shift 2 ;;
            --out) output="${2:?--out needs a value}"; shift 2 ;;
            --tier) tier="${2:?--tier needs a value}"; shift 2 ;;
            --days) days="${2:?--days needs a value}"; shift 2 ;;
            --max-agents) max_agents="${2:?--max-agents needs a value}"; shift 2 ;;
            --signer) SIGNER="${2:?--signer needs a value}"; shift 2 ;;
            *) fail "unknown license option '$1'" ;;
        esac
    done
    [[ -n "$keys" && -n "$fingerprint" && -n "$output" ]] || fail "license needs --keys, --fingerprint and --out"
    [[ -f "$keys/license-issuer" ]] || fail "--keys has no TEST licence issuer (made by an older '$0 keys'; run it again)"
    case "$tier" in
        self_host_pro|partner|oem) ;;
        *) fail "--tier must be self_host_pro, partner or oem (the only tiers the isolation installer admits)" ;;
    esac
    if [[ -e "$output" || -L "$output" ]]; then fail "--out must not exist yet"; fi
    local parent
    parent="$(cd -- "$(dirname -- "$output")" && pwd -P)" || fail "--out parent does not exist"
    case "$parent/" in
        "$ROOT"/*) fail "--out must be outside the repository (TEST licences never enter the workspace)" ;;
    esac
    ensure_signer
    local -a extra=()
    if [[ -n "$max_agents" ]]; then extra=("$max_agents"); fi
    "$SIGNER" license "$keys/license-issuer" "$parent/$(basename -- "$output")" "$tier" "$fingerprint" \
        cus_isolation_TEST sub_isolation_TEST "$days" "${extra[@]+"${extra[@]}"}"
    echo "Copy it to /root/duduclaw-migrate/license.json on the VM (root, 0600); it verifies only in a"
    echo "TEST-licence duduclaw built with $(cat -- "$keys/license-issuer.pub")."
}

case "${1:-}" in
    keys) shift; cmd_keys "$@" ;;
    bundle) shift; cmd_bundle "$@" ;;
    license) shift; cmd_license "$@" ;;
    -h|--help) usage ;;
    *) usage >&2; exit 2 ;;
esac
