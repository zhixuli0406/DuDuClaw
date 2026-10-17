#!/usr/bin/env python3
"""Verify security backports against checksum-pinned official crate archives."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
PROVENANCE_SHA256 = {
    "tauri-plugin-updater": "8f9f0d81eb924cccfb1a5176bed5828acbd88108d776123fa1bfe725a153b415",
    "calamine": "6c379343020964af247b59a5314de5295198044c5f14f98ca2516275c1f8e26b",
    "glib": "de7a59cce6352ea2913ae5c1be1306909b592b019c5cbf911a78c8790dd66ed4",
    "serenity": "3c1726725fd45dc5cdb389ff93b535b14a5df69cd573977936d0aa25d5fbeb13",
}
PACKAGES = {
    "tauri-plugin-updater": (ROOT / "src-tauri/vendor/tauri-plugin-updater", "2.10.1", "806d9dac662c2e4594ff03c647a552f2c9bd544e7d0f683ec58f872f952ce4af"),
    "calamine": (ROOT / "vendor/calamine", "0.36.1", "5fa68281b1a76b54a62156474adb06bb380a67e07dd60656e3217152b42183f3"),
    "glib": (ROOT / "src-tauri/vendor/glib", "0.18.5", "233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5"),
    "serenity": (ROOT / "vendor/serenity", "0.12.5", "9bde37f42765dfdc34e2a039e0c84afbf79a3101c1941763b0beb816c2f17541"),
}


def verify(name, cache_dir):
    directory, version, checksum = PACKAGES[name]
    archive_name = f"{name}-{version}.crate"
    if cache_dir:
        archive = (cache_dir / archive_name).read_bytes()
    else:
        url = f"https://static.crates.io/crates/{name}/{archive_name}"
        with urllib.request.urlopen(url, timeout=30) as response:
            archive = response.read(10 * 1024 * 1024 + 1)
    if hashlib.sha256(archive).hexdigest() != checksum:
        raise ValueError(f"{name}: upstream archive checksum mismatch")
    provenance = (directory / "SECURITY-PATCH.json").read_bytes()
    if hashlib.sha256(provenance).hexdigest() != PROVENANCE_SHA256[name]:
        raise ValueError(f"{name}: unexpected security patch specification")
    metadata = json.loads(provenance)
    if metadata["base_version"] != version or metadata["base_crate_sha256"] != checksum:
        raise ValueError(f"{name}: provenance does not match pinned source")
    replacements = metadata["replacements"]
    with tarfile.open(fileobj=io.BytesIO(archive)) as source:
        originals = {
            member.name.removeprefix(f"{name}-{version}/"): source.extractfile(member).read()
            for member in source.getmembers()
            if member.isfile()
        }
    verified = set()
    for path in directory.rglob("*"):
        if not path.is_file() or "target" in path.relative_to(directory).parts:
            continue
        relative = path.relative_to(directory).as_posix()
        if relative in {"SECURITY-PATCH.json", "Cargo.lock"}:
            continue
        if relative not in originals:
            raise ValueError(f"{name}: unexpected vendored file: {relative}")
        expected = originals[relative]
        for redaction in metadata.get("line_redactions", {}).get(relative, []):
            lines = expected.splitlines(keepends=True)
            line_number = redaction["line"]
            if not isinstance(line_number, int) or not 1 <= line_number <= len(lines):
                raise ValueError(f"{name}: invalid redaction line: {relative}")
            index = line_number - 1
            if hashlib.sha256(lines[index]).hexdigest() != redaction["original_sha256"]:
                raise ValueError(f"{name}: redaction source mismatch: {relative}")
            lines[index] = redaction["replacement"].encode()
            expected = b"".join(lines)
        for before, after in replacements.get(relative, []):
            before, after = before.encode(), after.encode()
            if expected.count(before) != 1:
                raise ValueError(f"{name}: replacement is not unique: {relative}")
            expected = expected.replace(before, after)
        if path.read_bytes() != expected:
            raise ValueError(f"{name}: unexpected change: {relative}")
        verified.add(relative)
    required = set(originals) - {"Cargo.lock"}
    required |= {p for p in originals if p.startswith(("LICENSE", "COPYRIGHT"))}
    if not required.issubset(verified) or not (set(replacements) | set(metadata.get("line_redactions", {}))).issubset(verified):
        raise ValueError(f"{name}: required upstream source or license is missing")
    print(f"{name} {version}: {len(verified)} files match official source and explicit security patches")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path)
    args = parser.parse_args()
    for name in PACKAGES:
        verify(name, args.cache_dir)


if __name__ == "__main__":
    main()
