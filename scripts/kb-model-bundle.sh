#!/usr/bin/env bash
# Build an offline bundle of a knowledge-base embedding model for one
# platform: the pinned model files plus the pinned ONNX Runtime archive,
# laid out exactly as they sit under a DuDuClaw home.
#
#   scripts/kb-model-bundle.sh <rust-target> <catalog-id> <out-dir>
#   scripts/kb-model-bundle.sh x86_64-unknown-linux-gnu multilingual-e5-small dist/
#
# Output: <out-dir>/duduclaw-kb-model-<catalog-id>-<rust-target>.tar and a
# matching .sha256. Every downloaded byte is checked against the pins in
# crates/duduclaw-knowledge/embed-catalog.json and
# crates/duduclaw-onnx/ort-archives.json (the same pins the binary checks);
# a mismatch aborts.
#
# Install on an offline machine: copy the .tar over, then
#   tar -xf duduclaw-kb-model-<id>-<target>.tar -C ~/.duduclaw
# and press "Check again (offline install)" in the dashboard
# (knowledge.embedding.verify_offline), which re-hashes every file, extracts
# the runtime library and only then marks the model installed.
#
# KB_BUNDLE_CACHE=<dir> keeps downloads between runs (default: a temp dir).
set -euo pipefail

if [ $# -ne 3 ]; then
  sed -n '2,19p' "$0" >&2
  exit 2
fi
TARGET="$1"
MODEL="$2"
OUT="$3"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CATALOG="$ROOT/crates/duduclaw-knowledge/embed-catalog.json"
ORT_JSON="$ROOT/crates/duduclaw-onnx/ort-archives.json"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Resolve the pins into a plain list: "<url>\t<dest>\t<size>\t<sha256>".
PLAN="$(python3 -I - "$CATALOG" "$ORT_JSON" "$TARGET" "$MODEL" <<'PY'
import json, sys
catalog, ort, target, model = sys.argv[1:5]
models = {m["id"]: m for m in json.load(open(catalog))["models"]}
if model not in models:
    sys.exit(f"unknown catalog id {model!r}; known: {', '.join(models)}")
m = models[model]
o = json.load(open(ort))
arch = next((a for a in o["archives"] if a["target"] == target), None)
if arch is None:
    sys.exit(f"no pinned ONNX Runtime for {target!r}")
for f in m["files"]:
    url = f"https://huggingface.co/{m['repo']}/resolve/{m['revision']}/{f['path']}"
    print(f"{url}\tmodels/embedding/{m['id']}/{f['path']}\t{f['size']}\t{f['sha256']}")
v = o["ort_version"]
print(f"{arch['url']}\tlib/onnxruntime/{v}/onnxruntime-{v}.{arch['kind']}\t{arch['size']}\t{arch['sha256']}")
PY
)"

CACHE="${KB_BUNDLE_CACHE:-$(mktemp -d)}"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"; [ -n "${KB_BUNDLE_CACHE:-}" ] || rm -rf "$CACHE"' EXIT
mkdir -p "$OUT"

while IFS=$'\t' read -r url dest size sum; do
  cached="$CACHE/$sum"
  if [ ! -f "$cached" ] || [ "$(sha256 "$cached")" != "$sum" ]; then
    echo "download $(basename "$dest") ($size bytes)"
    curl -fSL --retry 3 -o "$cached.part" "$url"
    mv "$cached.part" "$cached"
  fi
  got="$(wc -c < "$cached" | tr -d ' ')"
  [ "$got" = "$size" ] || { echo "size mismatch for $dest: $got != $size" >&2; exit 1; }
  [ "$(sha256 "$cached")" = "$sum" ] || { echo "sha256 mismatch for $dest" >&2; exit 1; }
  mkdir -p "$STAGE/$(dirname "$dest")"
  cp "$cached" "$STAGE/$dest"
done <<< "$PLAN"

python3 -I - "$STAGE/models/embedding/$MODEL/bundle.json" "$MODEL" "$TARGET" <<PY
import json, sys
path, model, target = sys.argv[1:4]
plan = """$PLAN""".strip().splitlines()
files = [dict(zip(["url", "path", "size", "sha256"], l.split("\t"))) for l in plan]
for f in files:
    f["size"] = int(f["size"])
json.dump({"model": model, "target": target, "files": files}, open(path, "w"), indent=2)
PY

NAME="duduclaw-kb-model-${MODEL}-${TARGET}.tar"
# Only the two trees: no "./" entry, so unpacking never touches the
# permissions of the home directory itself.
tar -cf "$OUT/$NAME" -C "$STAGE" models lib
(cd "$OUT" && sha256 "$NAME" | awk -v n="$NAME" '{print $1"  "n}' > "$NAME.sha256")
echo "wrote $OUT/$NAME"
cat "$OUT/$NAME.sha256"
