#!/usr/bin/env bash
# Build the DuDuClaw OS appliance image (mkosi disk image).
#
# Two independent build phases:
#   1. Produce a Linux `duduclaw` release binary matching APPLIANCE_ARCH
#      (the image target defaults to Debian/x86-64 — the shipping target —
#      regardless of build host; on macOS, `cargo build --release` alone
#      produces a Mach-O binary that's useless inside the image either way).
#   2. Feed that binary into mkosi via --extra-tree=<path>:/usr/local/bin/
#      duduclaw (ExtraTrees= "SOURCE:TARGET" colon-pair CLI syntax,
#      verified against the upstream mkosi manual, 2026-08) and run the
#      actual mkosi build — natively on Linux, via Docker (Dockerfile.
#      mkosi-runner) everywhere else.
#
# Usage:
#   appliance/build.sh                 # builds the binary via Docker, then the image
#   DUDUCLAW_BIN_PATH=/path/to/duduclaw appliance/build.sh   # skip step 1, use an existing Linux binary
#
# Environment variables:
#   APPLIANCE_ARCH=x86-64|arm64  Target architecture (default: x86-64, the
#                                 shipping target). x86-64 is what
#                                 mkosi.conf's [Distribution] Architecture=
#                                 default expects — matches step-1's docker
#                                 build --platform and step-2's mkosi
#                                 --architecture= override. arm64 is for
#                                 building+booting natively on an Apple
#                                 Silicon host (no cross-arch QEMU emulation
#                                 penalty) for local smoke testing only —
#                                 see README.md for which path to use when.
#   CARGO_JOBS=<n>                Caps cargo's build parallelism inside
#                                 step 1's rust-builder Docker stage
#                                 (container/Dockerfile.server's
#                                 CARGO_JOBS build-arg). Defaults to 2 here
#                                 — Docker Desktop's default VM (8GB/4CPU)
#                                 OOMs (SIGKILL, "cannot allocate memory")
#                                 compiling duduclaw-cli + duduclaw-gateway
#                                 in parallel at cargo's own default
#                                 (num-cpus) job count. Raise it if your
#                                 Docker Desktop VM has more memory
#                                 allocated (see README.md).
#
# Not yet run end-to-end as of this writing (needs a Linux environment +
# long package downloads) — reviewed for syntax (`bash -n`) and
# cross-checked against mkosi's documented CLI/config surface, not
# live-executed. See README.md "Known open points" before relying on it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPLIANCE_DIR="$REPO_ROOT/appliance"
OUTPUT_DIR="$APPLIANCE_DIR/mkosi.output"

APPLIANCE_ARCH="${APPLIANCE_ARCH:-x86-64}"
CARGO_JOBS="${CARGO_JOBS:-2}"

# Docker's --platform vocabulary (linux/amd64, linux/arm64) differs from
# mkosi's Architecture= vocabulary (x86-64, arm64) — both are the upstream
# CLI-documented spellings for each tool, so this maps between them rather
# than picking one tool's spelling and hoping the other accepts it too.
case "$APPLIANCE_ARCH" in
    x86-64) DOCKER_PLATFORM="linux/amd64" ;;
    arm64)  DOCKER_PLATFORM="linux/arm64" ;;
    *)
        echo "[build] unsupported APPLIANCE_ARCH=$APPLIANCE_ARCH (expected x86-64 or arm64)" >&2
        exit 1
        ;;
esac

echo "[build] repo root: $REPO_ROOT"
echo "[build] target architecture: $APPLIANCE_ARCH (docker platform: $DOCKER_PLATFORM, cargo jobs: $CARGO_JOBS)"

# --- Step 1: Linux duduclaw binary (arch = $APPLIANCE_ARCH) ---------------
if [[ -n "${DUDUCLAW_BIN_PATH:-}" ]]; then
    if [[ ! -f "$DUDUCLAW_BIN_PATH" ]]; then
        echo "[build] DUDUCLAW_BIN_PATH=$DUDUCLAW_BIN_PATH does not exist" >&2
        exit 1
    fi
    echo "[build] (1/2) using existing binary: $DUDUCLAW_BIN_PATH"
    echo "[build]       (caller-supplied — not verified to match APPLIANCE_ARCH=$APPLIANCE_ARCH)"
else
    echo "[build] (1/2) building linux/$APPLIANCE_ARCH duduclaw release binary via Docker"
    echo "[build]       (reusing container/Dockerfile.server's rust-builder stage —"
    echo "[build]        same pipeline scripts/release.sh already relies on for"
    echo "[build]        linux-x64 pro-binary extraction; no separate build recipe"
    echo "[build]        to keep in sync)"
    # --platform pins the build to $DOCKER_PLATFORM instead of defaulting to
    # the Docker daemon's host platform — without this, a build on Apple
    # Silicon silently produced an aarch64 binary even when the target
    # image (mkosi.conf's [Distribution] Architecture=x86-64) was amd64,
    # which is a binary the built image cannot execute (wrong ELF machine
    # type). --build-arg CARGO_JOBS threads through to Dockerfile.server's
    # ARG CARGO_JOBS="" (see that file) to cap parallel rustc/linker jobs —
    # Docker Desktop's default VM (8GB/4CPU) OOM-kills (SIGKILL) an
    # uncapped `cargo build --release -p duduclaw-cli -p duduclaw-gateway`
    # partway through linking duduclaw-gateway.
    docker build \
        --platform "$DOCKER_PLATFORM" \
        --build-arg "CARGO_JOBS=$CARGO_JOBS" \
        --build-arg "EXTRA_PACKAGES=-p duduclaw-sysd" \
        --target rust-builder \
        -t duduclaw-appliance-rust-builder \
        -f "$REPO_ROOT/container/Dockerfile.server" \
        "$REPO_ROOT"
    CID="$(docker create duduclaw-appliance-rust-builder)"
    trap 'docker rm -f "$CID" >/dev/null 2>&1 || true' EXIT
    mkdir -p "$APPLIANCE_DIR/.build"
    DUDUCLAW_BIN_PATH="$APPLIANCE_DIR/.build/duduclaw"
    docker cp "$CID:/build/target/release/duduclaw" "$DUDUCLAW_BIN_PATH"
    # The privileged helper ships alongside the gateway (duduclaw-sysd.service
    # runs it as root; see mkosi.extra/etc/systemd/system/duduclaw-sysd.service).
    docker cp "$CID:/build/target/release/duduclaw-sysd" "$APPLIANCE_DIR/.build/duduclaw-sysd"
    docker rm -f "$CID" >/dev/null 2>&1 || true
    trap - EXIT
    echo "[build]       extracted: $DUDUCLAW_BIN_PATH (+ duduclaw-sysd)"
fi

# --- Step 1b: vendor the AI CLIs (network here, not in mkosi postinst) -----
# claude-code/codex/gemini-cli come from npm, and mkosi's postinst sandbox
# has no network — so fetch them in a Docker build (which does) and hand the
# resulting /usr/local tree to mkosi as an ExtraTree. --platform must match
# APPLIANCE_ARCH because claude-code ships a native arch-specific launcher.
CLIS_TREE="$APPLIANCE_DIR/.build/clis"
if [[ -z "${SKIP_CLI_VENDOR:-}" ]]; then
    echo "[build] (1b) vendoring AI CLIs (claude-code/codex/gemini-cli) for $APPLIANCE_ARCH"
    docker build \
        --platform "$DOCKER_PLATFORM" \
        -t duduclaw-appliance-cli-vendor \
        -f "$APPLIANCE_DIR/Dockerfile.cli-vendor" \
        "$APPLIANCE_DIR"
    VCID="$(docker create --platform "$DOCKER_PLATFORM" duduclaw-appliance-cli-vendor)"
    trap 'docker rm -f "$VCID" >/dev/null 2>&1 || true' EXIT
    rm -rf "$CLIS_TREE"
    mkdir -p "$CLIS_TREE"
    docker cp "$VCID:/clis/." "$CLIS_TREE/"
    docker rm -f "$VCID" >/dev/null 2>&1 || true
    trap - EXIT
    echo "[build]       vendored: $CLIS_TREE (→ /usr/local in image)"
else
    echo "[build] (1b) SKIP_CLI_VENDOR set — image will lack the AI CLIs"
fi

# --- Step 1c: DuDuClaw OS gpui shell (optional, Shell-S2 wave) -------------
# The native shell is built from the detached `crates/duduclaw-shell`
# workspace (gpui pulls the whole zed monorepo — deliberately NOT rebuilt
# by this script; the one-shot container build lives in
# crates/duduclaw-shell/BUILD-LINUX.md). Pass a prebuilt Linux binary in:
#   DUDUCLAW_SHELL_BIN_PATH=/path/to/duduclaw-shell appliance/build.sh
# When set, the image ships it at /usr/local/bin/duduclaw-shell and the
# kiosk session runs the shell instead of the Chromium dashboard (see
# mkosi.extra/usr/local/sbin/duduclaw-kiosk-launch.sh's selection order,
# incl. the /etc/duduclaw/kiosk-app escape hatch). When unset, nothing
# changes: Chromium kiosk, byte-identical to before this step existed.
if [[ -n "${DUDUCLAW_SHELL_BIN_PATH:-}" ]]; then
    if [[ ! -f "$DUDUCLAW_SHELL_BIN_PATH" ]]; then
        echo "[build] DUDUCLAW_SHELL_BIN_PATH=$DUDUCLAW_SHELL_BIN_PATH does not exist" >&2
        exit 1
    fi
    echo "[build] (1c) shell binary: $DUDUCLAW_SHELL_BIN_PATH → /usr/local/bin/duduclaw-shell (kiosk runs the gpui shell)"
    echo "[build]       (caller-supplied — not verified to match APPLIANCE_ARCH=$APPLIANCE_ARCH)"
else
    echo "[build] (1c) DUDUCLAW_SHELL_BIN_PATH unset — kiosk stays on the Chromium dashboard"
fi

# --- Step 2: mkosi build ---------------------------------------------------
mkdir -p "$OUTPUT_DIR"

HOST_OS="$(uname -s)"
if [[ "$HOST_OS" == "Linux" ]]; then
    echo "[build] (2/2) native Linux host — running mkosi directly"
    if ! command -v mkosi >/dev/null 2>&1; then
        echo "[build] mkosi not found on PATH. Install it first, e.g.:" >&2
        echo "[build]   apt-get install mkosi   (Debian/Ubuntu)" >&2
        echo "[build]   dnf install mkosi        (Fedora)" >&2
        exit 1
    fi
    # --architecture= is passed explicitly (not left to mkosi.conf's own
    # [Distribution] Architecture=x86-64 default) so the actual target is
    # never implicit — CLI-configured settings always override
    # file-configured ones (mkosi manual, config parsing order), so this
    # is safe to pass even when APPLIANCE_ARCH matches the file default.
    # OPTIONS BEFORE THE VERB (bitten for real, 2026-08-18): mkosi's usage
    # is `mkosi [options…] build [command line…]` — anything AFTER `build`
    # is treated as the build-script command line and silently swallowed,
    # which produced a full amd64 tree from an "--architecture=arm64" run.
    NATIVE_CLI_TREE_ARG=()
    if [[ -d "$CLIS_TREE" ]]; then
        NATIVE_CLI_TREE_ARG=("--extra-tree=${CLIS_TREE}:/usr/local")
    fi
    NATIVE_SHELL_TREE_ARG=()
    if [[ -n "${DUDUCLAW_SHELL_BIN_PATH:-}" ]]; then
        NATIVE_SHELL_TREE_ARG=("--extra-tree=${DUDUCLAW_SHELL_BIN_PATH}:/usr/local/bin/duduclaw-shell")
    fi
    # APPLIANCE_DEBUG=1 sets a root password so you can log in on the serial
    # console (`root` / the value of APPLIANCE_DEBUG_PASSWORD, default
    # "duduclaw") to inspect the running VM — journalctl, systemctl status,
    # etc. DEFAULT OFF: a shipping image keeps root locked (no password).
    DEBUG_ARGS=()
    if [[ -n "${APPLIANCE_DEBUG:-}" ]]; then
        DEBUG_ARGS=(--root-password="${APPLIANCE_DEBUG_PASSWORD:-duduclaw}")
        echo "[build] APPLIANCE_DEBUG set — serial root login enabled (NOT for shipping)"
    fi
    # --force: rebuild even if mkosi.output/ already holds a previous image
    # (otherwise mkosi prints "Output path ... exists already" and skips).
    (cd "$APPLIANCE_DIR" && mkosi \
        --force \
        "--architecture=${APPLIANCE_ARCH}" \
        "--extra-tree=${DUDUCLAW_BIN_PATH}:/usr/local/bin/duduclaw" \
        "${NATIVE_CLI_TREE_ARG[@]}" \
        "${NATIVE_SHELL_TREE_ARG[@]}" \
        "${DEBUG_ARGS[@]}" \
        build)
else
    echo "[build] (2/2) non-Linux host ($HOST_OS) — running mkosi inside Docker"
    echo "[build]       (Dockerfile.mkosi-runner; --privileged for loop/mount access —"
    echo "[build]        RepartOffline= defaults to enabled, which mkosi's own docs say"
    echo "[build]        should avoid *needing* this, but Docker Desktop's VM makes"
    echo "[build]        --privileged cheap insurance rather than something to prove"
    echo "[build]        unnecessary on the first attempt)"
    docker build -t duduclaw-mkosi-runner -f "$APPLIANCE_DIR/Dockerfile.mkosi-runner" "$APPLIANCE_DIR"
    # NO bind mounts here, on purpose (first real run, 2026-08-18): Docker
    # Desktop's macOS file sharing misreports permission bits on mounted
    # files (mkosi.version showed up executable inside the container, so
    # mkosi tried to *run* it per its exec-bit rule and died on
    # PermissionError) — and the same lie would leak into the built image's
    # file modes via ExtraTrees. `docker cp` streams a tar with the host's
    # true modes, so we copy the sources in, run mkosi against the
    # container's own filesystem, and copy the output back out.
    # OPTIONS BEFORE THE VERB — same trap as the native branch above:
    # args after `build` are silently swallowed as the build-script
    # command line (this produced one full amd64 image from an arm64 run
    # before being caught).
    # APPLIANCE_DEBUG=1 → serial root login for VM inspection (default off,
    # shipping image keeps root locked). See the native branch above.
    DEBUG_ARGS=()
    if [[ -n "${APPLIANCE_DEBUG:-}" ]]; then
        DEBUG_ARGS=(--root-password="${APPLIANCE_DEBUG_PASSWORD:-duduclaw}")
        echo "[build] APPLIANCE_DEBUG set — serial root login enabled (NOT for shipping)"
    fi
    CID="$(docker create --privileged \
        -w /workspace/appliance \
        duduclaw-mkosi-runner \
        --force \
        "--architecture=${APPLIANCE_ARCH}" \
        "--extra-tree=/workspace/bin-tree:/" \
        "${DEBUG_ARGS[@]}" \
        build)"
    trap 'docker rm -f "$CID" >/dev/null 2>&1 || true' EXIT
    # Copy ONLY the mkosi sources into the container — NOT mkosi.output/ (the
    # 12.5G raw from a prior run) or .build/ (binaries + logs). Copying those
    # in both wasted the VM's disk (a second 12.5G raw) and made mkosi see an
    # existing output and skip the build. rsync --exclude stages a clean tree.
    SRC_STAGE="$(mktemp -d)"
    # `.vm` joined the exclude list 2026-08-20: run-vm.sh keeps its 13G
    # persistent working disk (plus logs/screenshots) there, and copying it
    # into the runner container filled Docker Desktop's VM disk ("no space
    # left on device") — the dir didn't exist yet when this staging step was
    # first written, so the trap only fired once VM testing had happened.
    rsync -a --exclude 'mkosi.output' --exclude '.build' --exclude '.vm' \
        "$APPLIANCE_DIR/" "$SRC_STAGE/appliance/"
    # mkosi writes into mkosi.output/ only if that dir EXISTS; since the
    # rsync above excluded the (stale, huge) host copy, recreate it empty so
    # the build output lands where the docker cp-back below looks for it —
    # otherwise mkosi writes duduclaw-os.raw into the CWD and the copy-back
    # finds nothing.
    mkdir -p "$SRC_STAGE/appliance/mkosi.output"
    docker cp "$SRC_STAGE/appliance" "$CID:/workspace"    # → /workspace/appliance
    rm -rf "$SRC_STAGE"
    BIN_TREE="$(mktemp -d)"
    mkdir -p "$BIN_TREE/usr/local/bin"
    cp "$DUDUCLAW_BIN_PATH" "$BIN_TREE/usr/local/bin/duduclaw"
    chmod 755 "$BIN_TREE/usr/local/bin/duduclaw"
    if [[ -f "$APPLIANCE_DIR/.build/duduclaw-sysd" ]]; then
        cp "$APPLIANCE_DIR/.build/duduclaw-sysd" "$BIN_TREE/usr/local/bin/duduclaw-sysd"
        chmod 755 "$BIN_TREE/usr/local/bin/duduclaw-sysd"
    else
        # Caller-supplied DUDUCLAW_BIN_PATH path skips step 1, so the helper
        # may be absent — the unit would then fail visibly at boot. Warn, do
        # not fabricate.
        echo "[build] WARNING: duduclaw-sysd binary not found; image will lack the privileged helper" >&2
    fi
    # Fold the vendored AI CLIs into the same staging tree at /usr/local
    # (step 1b produced /clis with bin/ + lib/node_modules/; cp -a preserves
    # the relative symlinks so /usr/local/bin/{claude,codex,gemini} resolve).
    if [[ -d "$CLIS_TREE" ]]; then
        cp -a "$CLIS_TREE/." "$BIN_TREE/usr/local/"
    fi
    # Step 1c: the gpui shell binary, when supplied (see the validation
    # block above step 2 — the kiosk launch script switches on its presence).
    if [[ -n "${DUDUCLAW_SHELL_BIN_PATH:-}" ]]; then
        cp "$DUDUCLAW_SHELL_BIN_PATH" "$BIN_TREE/usr/local/bin/duduclaw-shell"
        chmod 755 "$BIN_TREE/usr/local/bin/duduclaw-shell"
    fi
    docker cp "$BIN_TREE" "$CID:/workspace/bin-tree"      # → /workspace/bin-tree/usr/local/bin/duduclaw
    rm -rf "$BIN_TREE"
    docker start -a "$CID" || { docker rm -f "$CID" >/dev/null 2>&1; trap - EXIT; echo "[build] mkosi failed" >&2; exit 1; }
    mkdir -p "$OUTPUT_DIR"
    docker cp "$CID:/workspace/appliance/mkosi.output/." "$OUTPUT_DIR/"
    docker rm -f "$CID" >/dev/null 2>&1 || true
    trap - EXIT
fi

echo "[build] done. Output in: $OUTPUT_DIR"
ls -la "$OUTPUT_DIR" 2>/dev/null || true
