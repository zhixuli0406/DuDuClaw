#!/usr/bin/env python3
"""Reject release archives that cannot be installed by legacy self-updaters."""

import argparse
from pathlib import Path
import sys
import tarfile
import zipfile

# <=1.79.2 rejects an extracted binary at >=200 MiB, even when the NEW
# updater has a larger limit. Keep this until upgrades from those versions
# are no longer supported (or require an explicit manual bridge upgrade).
LEGACY_BINARY_LIMIT = 209_715_200
BINARY_NAMES = {"duduclaw", "duduclaw.exe", "duduclaw-pro", "duduclaw-pro.exe"}


def check_member(name: str, size: int, regular: bool) -> bool:
    # Validate every member, including files the updater would otherwise skip.
    parts = name.replace("\\", "/").split("/")
    if name.startswith(("/", "\\")) or ".." in parts or (
        len(name) >= 2 and name[0].isalpha() and name[1] == ":"
    ):
        raise ValueError(f"{name}: unsafe archive member path")
    if parts[-1] not in BINARY_NAMES:
        return False
    # Releases put binaries at the root; tar commonly prefixes them with './'.
    if "\\" in name or any(part != "." for part in parts[:-1]):
        raise ValueError(f"{name}: update binary must be at archive root")
    if not regular:
        raise ValueError(f"{name}: update binary must be a regular file")
    if size >= LEGACY_BINARY_LIMIT:
        raise ValueError(
            f"{name}: {size:,} bytes must be < {LEGACY_BINARY_LIMIT:,} bytes "
            "for <=1.79.2 updaters; strip symbols or reduce the binary before release"
        )
    return True


def check_archive(path: Path) -> None:
    found = False
    if path.name.endswith(".tar.gz"):
        with tarfile.open(path, "r:gz") as archive:
            for entry in archive:
                found = check_member(entry.name, entry.size, entry.isfile()) or found
            # tar stops at the end marker before gzip's trailer. Verify that too.
            while archive.fileobj.read(64 * 1024):
                pass
    elif path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            for entry in archive.infolist():
                mode = (entry.external_attr >> 16) & 0o170000
                regular = not entry.is_dir() and mode in (0, 0o100000)
                found = check_member(entry.filename, entry.file_size, regular) or found
    else:
        raise ValueError(f"unsupported update archive: {path}")
    if not found:
        raise ValueError(f"{path}: no duduclaw update binary found")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archives", type=Path, nargs="+")
    args = parser.parse_args()
    try:
        for path in args.archives:
            check_archive(path)
    except (OSError, EOFError, ValueError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
