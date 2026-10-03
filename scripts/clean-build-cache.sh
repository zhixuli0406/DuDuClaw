#!/usr/bin/env bash
# Reclaim disk from this workspace's own build artifacts, keeping third-party
# dependency artifacts (the slow part to rebuild).
#
#   scripts/clean-build-cache.sh [--dry-run] [--force] [--all-profiles]
#
#   --dry-run       print what would be removed, with sizes; delete nothing
#   --force         run even when a cargo or rustc process is alive
#   --all-profiles  also clean target/release and other target triples
#                   (same rules: only this workspace's crates)
#
# Removes under target/debug: deps/duduclaw*, deps/libduduclaw*,
# .fingerprint/duduclaw*, build/duduclaw*, examples, incremental, and the
# top-level duduclaw* / libduduclaw* outputs.
# A top-level binary that a running process was started from is kept.
# Background: target/debug once reached 134 GB and a full disk crashed Docker
# Desktop. Run this when `df` shows less than ~100 GB free before heavy builds.
set -euo pipefail

DRY=0; FORCE=0; ALL=0
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --force) FORCE=1 ;;
    --all-profiles) ALL=1 ;;
    -h|--help) sed -n 2,17p "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
TARGET="$ROOT/target"
[ -d "$TARGET" ] || { echo "no $TARGET, nothing to clean"; exit 0; }

free() { df -h "$TARGET" | awk 'NR==2 {print $4 " free (" $5 " used)"}'; }
size() { du -sk "$@" 2>/dev/null | awk '{s+=$1} END {printf "%.1f MB", s/1024}'; }

echo "free space : $(free)"
[ -d "$TARGET/debug" ] && echo "target/debug: $(du -sh "$TARGET/debug" | awk '{print $1}')"

busy="$( { pgrep -x -l cargo; pgrep -x -l rustc; } 2>/dev/null || true)"
if [ -n "$busy" ] && [ "$FORCE" -eq 0 ]; then
  echo "refusing: a cargo or rustc process is running (wait for it, or use --force):" >&2
  echo "$busy" >&2
  exit 1
fi

# Profile directories to clean. Other target triples have the same layout
# (target/<triple>/debug|release); target/release only with --all-profiles.
profiles=("$TARGET/debug")
if [ "$ALL" -eq 1 ]; then
  profiles+=("$TARGET/release")
  for d in "$TARGET"/*/; do
    case "$(basename "$d")" in debug|release|tmp) continue ;; esac
    for p in debug release; do [ -d "$d$p" ] && profiles+=("$d$p"); done
  done
fi

removed_any=0
total_kb=0
# A binary some process was started from (for example a gateway launched
# from target/release) is kept: deleting it would break that process's next
# restart.
in_use() {
  [ -f "$1" ] || return 1
  ps -axo command= | awk -v p="$1" 'index($0, p) == 1 { found = 1 } END { exit !found }'
}
remove() {
  # $1 = path; never leaves target/
  case "$1" in "$TARGET"/*) ;; *) echo "internal error: $1 outside target" >&2; exit 1 ;; esac
  [ -e "$1" ] || return 0
  if in_use "$1"; then
    printf 'kept (running) %s\n' "${1#"$ROOT"/}"
    return 0
  fi
  removed_any=1
  total_kb=$((total_kb + $(du -sk "$1" 2>/dev/null | awk '{print $1}')))
  if [ "$DRY" -eq 1 ]; then
    printf 'would remove  %-12s %s\n' "$(size "$1")" "${1#"$ROOT"/}"
  else
    rm -rf -- "$1"
    printf 'removed       %s\n' "${1#"$ROOT"/}"
  fi
}

for prof in "${profiles[@]}"; do
  [ -d "$prof" ] || continue
  echo "-- ${prof#"$ROOT"/}"
  shopt -s nullglob
  for pat in \
    "$prof"/deps/duduclaw* "$prof"/deps/libduduclaw* \
    "$prof"/.fingerprint/duduclaw* "$prof"/build/duduclaw* \
    "$prof"/examples "$prof"/incremental \
    "$prof"/duduclaw* "$prof"/libduduclaw*; do
    remove "$pat"
  done
  shopt -u nullglob
done

[ "$removed_any" -eq 1 ] || echo "nothing to remove"
if [ "$DRY" -eq 1 ]; then
  echo "total that would be removed: $((total_kb / 1024)) MB (dry run, nothing deleted)"
else
  echo "total removed: $((total_kb / 1024)) MB"
fi
echo "free space : $(free)"
