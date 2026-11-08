#!/usr/bin/env python3
"""Check the exact archived build identity before signing; never execute assets."""
import argparse
import re
import tarfile
import zipfile
from pathlib import Path

MAGIC = b"DUDUCLAW_RELEASE_ID"
HEADER = b"\0" + MAGIC + b":V1:"
SUFFIX = b"\0DUDUCLAW_RELEASE_END\0"
MAX_BINARY_BYTES = 768 * 1024 * 1024


# Load by path so these standalone scripts also work under importlib-based tests.
import importlib.util
_bundle_spec = importlib.util.spec_from_file_location(
    "macos_bundle_archive", Path(__file__).with_name("macos_bundle_archive.py"))
_bundle = importlib.util.module_from_spec(_bundle_spec)
_bundle_spec.loader.exec_module(_bundle)


def plain_version(version):
    return (re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version)
            is not None and all(int(part) <= 2**64 - 1 for part in version.split(".")))


def verify_marker(stream, expected):
    if not plain_version(expected):
        raise ValueError("invalid expected release version")
    window = b""
    scanned = 0
    found = False
    total = 0
    max_record = len(HEADER) + 62 + len(SUFFIX)
    while True:
        chunk = stream.read(64 * 1024)
        total += len(chunk)
        if total > MAX_BINARY_BYTES:
            raise ValueError("release binary exceeds size limit")
        window += chunk
        scan_end = max(0, len(window) - max_record) if chunk else len(window)
        offset = window.find(MAGIC, scanned)
        while offset != -1 and offset < scan_end:
            if found:
                raise ValueError("duplicate release marker")
            if offset == 0 or window[offset - 1:offset - 1 + len(HEADER)] != HEADER:
                raise ValueError("malformed release marker header")
            tail = window[offset - 1 + len(HEADER):]
            end = tail.find(b"\0")
            if end < 0 or end > 62 or tail[end:end + len(SUFFIX)] != SUFFIX:
                raise ValueError("malformed release marker suffix")
            actual = tail[:end].decode("ascii")
            if not plain_version(actual) or actual != expected:
                raise ValueError("binary marker does not match release tag")
            found = True
            offset = window.find(MAGIC, offset + 1)
        if not chunk:
            break
        discard = max(0, scan_end - 1)
        window = window[discard:]
        scanned = scan_end - discard
    if not found:
        raise ValueError("release marker missing")


def verify_archive(path, expected):
    # Release packaging uses root-level binary names. Never extract to disk.
    names = {"duduclaw", "duduclaw-pro", "duduclaw.exe", "duduclaw-pro.exe"}
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            entries = ([_bundle.bundle_executable(archive, path.name)]
                       if path.name.endswith(".app.zip") else
                       [entry for entry in archive.infolist() if entry.filename in names])
            if len(entries) != 1 or entries[0].is_dir():
                raise ValueError("archive must contain exactly one release binary")
            with archive.open(entries[0]) as binary:
                verify_marker(binary, expected)
    else:
        with tarfile.open(path, "r|gz") as archive:
            found = False
            for entry in archive:
                if entry.name not in names:
                    continue
                if found or not entry.isfile():
                    raise ValueError("archive must contain exactly one regular release binary")
                found = True
                with archive.extractfile(entry) as binary:
                    verify_marker(binary, expected)
            if not found:
                raise ValueError("release binary missing")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--archive", type=Path, required=True)
    args = parser.parse_args()
    if not args.tag.startswith("v") or not plain_version(args.tag[1:]):
        parser.error("tag must be vX.Y.Z")
    try:
        verify_archive(args.archive, args.tag[1:])
    except (ValueError, OSError, tarfile.TarError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Release identity check failed: {error}\n")
    print(f"Release identity verified: {args.archive.name} ({args.tag})")


if __name__ == "__main__":
    main()
