#!/usr/bin/env bash
# TEST-ONLY isolation key kit (plan §3.3, R2 §4.2; owner decision Q1: implementation
# and VM acceptance use test root keys, the real ceremony happens before release).
#
#   scripts/isolation-test-keys.sh keys    [--out <new dir>] [--signer <duduclaw-isolation-test-signer>]
#   scripts/isolation-test-keys.sh bundle  --keys <keys dir> --archive <duduclaw-linux-<arch>.tar.gz>
#                                          --target <triple> [--gateway <TEST duduclaw>]
#                                          [--policy-source <file>] [--policy-tool <path>]
#                                          [--out <new dir>] [--signer <path>] [--accept-mcp]
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
# Every generation needs a signed isolation policy (runtime-isolation-policy-v1):
# `bundle` extracts the archive's runtime/forwarder/bridge binaries into a private
# temporary directory, builds the policy generator (duduclaw-isolation-policy,
# feature policy-tool; Linux only, or pass a prebuilt --policy-tool), generates the
# policy from --policy-source (default: the TEST fixture
# scripts/live-test/linux-protected/fixtures/policy-source-test.json, one
# OpenAI-compatible egress runtime WITHOUT MCP acceptance: good for install and
# readiness acceptance, every MCP run of it is refused) into <out>/TEST-runtime,
# signs with the TEST template fixtures/manifest-template-test.json (the shipped
# template plus the policy and api-worker-v1 entries) and --runtime-artifacts-dir,
# then re-verifies the policy against the signed manifest.
# --accept-mcp (default off; R8r-4) first runs every open_ai_compat runtime of the
# source through `duduclaw-isolation-policy accept` (as root via `sudo -n`, with
# DUDUCLAW_ISO_DISPOSABLE=1: only on a disposable Linux host of the target
# architecture WITHOUT an isolation installation), builds from the accepted source,
# verifies with --evidence-dir and copies the evidence to <out>/TEST-runtime/
# acceptance-evidence/ (after packaging; never signed or packaged).
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
POLICY_TOOL=""
OUT=""
TEST_FIXTURES="$ROOT/scripts/live-test/linux-protected/fixtures"
# Output directory of this run; removed again if the run fails part-way, so a
# half-made TEST key set never lingers. Cleared once the run has succeeded.
CLEANUP_DIR=""
# Private temporary directory of `bundle`'s policy inputs; always removed.
POLICY_WORK=""
cleanup() {
    local status=$?
    if [[ -n "$POLICY_WORK" ]]; then rm -rf -- "$POLICY_WORK"; fi
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

# The offline policy generator (crates/duduclaw-spawnd, feature policy-tool; never a
# release binary). It runs spawnd's Linux-only policy readers, so it needs Linux.
ensure_policy_tool() {
    if [[ -n "$POLICY_TOOL" ]]; then
        [[ -x "$POLICY_TOOL" && -f "$POLICY_TOOL" ]] || fail "--policy-tool must be an executable file"
        return
    fi
    [[ "$(uname -s)" == "Linux" ]] || fail "the policy generator runs only on Linux; run bundle on a Linux host or pass --policy-tool"
    # Separate target directory, like the TEST signer: never mixed with release outputs.
    (cd -- "$ROOT" && CARGO_TARGET_DIR="$ROOT/target/isolation-policy-tool" \
        cargo build --locked --release -p duduclaw-spawnd --features policy-tool --bin duduclaw-isolation-policy)
    POLICY_TOOL="$ROOT/target/isolation-policy-tool/release/duduclaw-isolation-policy"
}

# The release archive's policy-generator inputs (runtime entries of the TEST template,
# egress forwarder, MCP bridge) under their generation names in the new directory $4,
# through the packager's own archive check and staging (snapshot_release_archive,
# stage_policy_inputs). With a non-empty $5 (R8r-4 --accept-mcp) the same snapshot's
# trampoline (duduclaw-run) is also copied to the new file $5 for `accept --trampoline`.
stage_policy_inputs() {
    python3 -I -B -c 'import importlib.util, shutil, sys, tempfile
from pathlib import Path
script, archive, target, template, destination, trampoline = sys.argv[1:7]
spec = importlib.util.spec_from_file_location("packager", script)
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)
with tempfile.TemporaryDirectory(prefix="snapshot-", dir=str(Path(destination).parent)) as snapshot:
    packager.snapshot_release_archive(archive, target, Path(snapshot), test_archive=True)
    for name in packager.stage_policy_inputs(Path(snapshot), Path(template), Path(destination)):
        print("  policy input " + name)
    if trampoline:
        source = Path(snapshot) / packager.INSTALL_BINARIES["trampoline"]
        packager.validate_binary(source)
        with source.open("rb") as reader, Path(trampoline).open("xb") as writer:
            shutil.copyfileobj(reader, writer)
        Path(trampoline).chmod(0o755)
        print("  trampoline " + trampoline)' "$ROOT/scripts/package-linux-release.py" "$1" "$2" "$3" "$4" "${5:-}"
}

# R8r-4: the labels of the open_ai_compat runtimes of policy source $1, one per line.
api_worker_labels() {
    python3 -I -B -c 'import json, sys
with open(sys.argv[1], "rb") as source:
    document = json.load(source)
for runtime in document.get("runtimes", []):
    if isinstance(runtime, dict) and runtime.get("runtime") == {"kind": "open_ai_compat"}:
        print(runtime["label"])' "$1"
}

# R8r-4: `duduclaw-isolation-policy accept` as root (sudo -n unless already root),
# with the disposable-host marker the root tests use.
run_accept() {
    if [[ "$(id -u)" -eq 0 ]]; then
        env DUDUCLAW_ISO_DISPOSABLE=1 "$POLICY_TOOL" accept "$@"
    else
        sudo -n env DUDUCLAW_ISO_DISPOSABLE=1 "$POLICY_TOOL" accept "$@"
    fi
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
    local keys="" archive="" target="" gateway="" policy_source="$TEST_FIXTURES/policy-source-test.json" accept_mcp=0
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --accept-mcp) accept_mcp=1; shift ;;
            --keys) keys="${2:?--keys needs a value}"; shift 2 ;;
            --archive) archive="${2:?--archive needs a value}"; shift 2 ;;
            --target) target="${2:?--target needs a value}"; shift 2 ;;
            --gateway) gateway="${2:?--gateway needs a value}"; shift 2 ;;
            --policy-source) policy_source="${2:?--policy-source needs a value}"; shift 2 ;;
            --policy-tool) POLICY_TOOL="${2:?--policy-tool needs a value}"; shift 2 ;;
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
    [[ -f "$policy_source" && ! -L "$policy_source" ]] || fail "--policy-source must be a regular file"
    ensure_signer
    ensure_policy_tool
    local out version tar_name expires template policy runtime_dir
    template="$TEST_FIXTURES/manifest-template-test.json"
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
    # The TEST isolation policy, generated from the archive's own bytes and checked by
    # spawnd's readers before it is written; the packager signs it as a runtime entry.
    POLICY_WORK="$(mktemp -d "${TMPDIR:-/tmp}/duduclaw-isolation-TEST-policy.XXXXXX")"
    chmod 0700 -- "$POLICY_WORK"
    local build_source="$policy_source" trampoline=""
    local -a evidence_args=()
    if [[ "$accept_mcp" -eq 1 ]]; then trampoline="$POLICY_WORK/duduclaw-run"; fi
    stage_policy_inputs "$archive" "$target" "$template" "$POLICY_WORK/artifacts" "$trampoline"
    if [[ "$accept_mcp" -eq 1 ]]; then
        # R8r-4: run every open_ai_compat runtime of the source through the MCP
        # acceptance harness (root, disposable host only), chaining the accepted
        # sources, and build from the last one.
        local label current next
        local -a labels=()
        while IFS= read -r label; do labels+=("$label"); done < <(api_worker_labels "$policy_source")
        [[ "${#labels[@]}" -gt 0 ]] || fail "--accept-mcp: the policy source has no open_ai_compat runtime"
        mkdir -m 0700 -- "$POLICY_WORK/acceptance"
        current="$policy_source"
        for label in "${labels[@]}"; do
            next="$POLICY_WORK/policy-source.accepted.$label.json"
            run_accept --source "$current" --artifacts-dir "$POLICY_WORK/artifacts" --arch "$arch" --label "$label" \
                --trampoline "$trampoline" --evidence-dir "$POLICY_WORK/acceptance/$label" --out-source "$next"
            current="$next"
        done
        # The harness writes its evidence root-owned (0700/0600); hand it back so the
        # checks below can read it and the cleanup can remove it.
        if [[ "$(id -u)" -ne 0 ]]; then
            sudo -n chown -R -- "$(id -u):$(id -g)" "$POLICY_WORK/acceptance"
        fi
        build_source="$current"
        evidence_args=(--evidence-dir "$POLICY_WORK/acceptance")
    fi
    runtime_dir="$out/TEST-runtime"
    mkdir -m 0700 -- "$runtime_dir"
    policy="$runtime_dir/runtime-isolation-policy-v1"
    "$POLICY_TOOL" build --source "$build_source" --artifacts-dir "$POLICY_WORK/artifacts" --arch "$arch" --out "$policy"
    echo "TEST-ONLY isolation policy (from $build_source): $policy"
    python3 "$ROOT/scripts/package-linux-release.py" --target "$target" --from-release-archive "$archive" \
        --manifest-template "$template" --manifest-valid-days 30 \
        --runtime-artifacts-dir "$runtime_dir" \
        --signing-key "$keys/target" --test-signer "$SIGNER" \
        --bootstrap-root "$keys/root.json" --bootstrap-root-signatures "$keys/root.json.signatures.json" \
        --output-dir "$out"
    # The same checks again, now against the signed manifest (it must bind these policy bytes).
    "$POLICY_TOOL" verify --policy "$policy" --artifacts-dir "$POLICY_WORK/artifacts" \
        --manifest "$out/isolated-linux-$arch-manifest.json" "${evidence_args[@]+"${evidence_args[@]}"}"
    if [[ "$accept_mcp" -eq 1 ]]; then
        # Next to the bundle output, after packaging: the packager reads only the
        # runtime-<registry_id> files of TEST-runtime and the migration bundle packs
        # only the bootstrap inbox, so this copy is never signed or packaged.
        cp -R -- "$POLICY_WORK/acceptance" "$runtime_dir/acceptance-evidence"
        echo "TEST-ONLY MCP acceptance evidence: $runtime_dir/acceptance-evidence"
    fi
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
