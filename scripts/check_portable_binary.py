#!/usr/bin/env python3
"""Fail if a release binary depends on libraries customers will not have.

* Mach-O: every dependency must live under /usr/lib/ or /System/Library/
  (or be @rpath/@loader_path/@executable_path relative). Uses `otool -L`.
* PE: the import table (and delay-load table) must not name an OpenSSL DLL.
  Parsed in pure Python; both the regular and the delay-load import
  directories are checked.
* ELF: skipped (Linux is covered by check_glibc.py and static vendoring).
"""

import argparse
from pathlib import Path
import re
import struct
import subprocess
import sys

MACHO_MAGICS = {
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",  # 32-bit
    b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",  # 64-bit
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",  # fat (big/little)
    b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",  # fat64
}
ALLOWED_PREFIXES = ("/usr/lib/", "/System/Library/")
RELATIVE_PREFIXES = ("@rpath/", "@loader_path/", "@executable_path/")
OPENSSL_DLL = re.compile(r"^(libssl|libcrypto|ssleay32|libeay32)", re.IGNORECASE)


def detect_format(data: bytes) -> str:
    if data[:4] == b"\x7fELF":
        return "elf"
    if data[:4] in MACHO_MAGICS:
        return "macho"
    if data[:2] == b"MZ":
        return "pe"
    return "unknown"


def macho_dependencies(path: Path) -> list:
    out = subprocess.run(
        ["otool", "-L", str(path)], capture_output=True, text=True, check=True
    ).stdout
    deps = []
    for line in out.splitlines():
        # Header lines end with ':' (one per arch); dependency lines are indented.
        if not line.startswith(("\t", " ")):
            continue
        deps.append(line.strip().split(" (compatibility version", 1)[0])
    return deps


def check_macho(path: Path) -> list:
    try:
        deps = macho_dependencies(path)
    except (OSError, subprocess.CalledProcessError) as error:
        return [f"{path}: otool -L failed: {error}"]
    bad = [
        d for d in deps
        if not d.startswith(ALLOWED_PREFIXES + RELATIVE_PREFIXES)
    ]
    return [f"{path}: non-system dylib dependency: {d}" for d in sorted(set(bad))]


def _sections(data: bytes, pe: int, count: int, opt_size: int, nt_size: int):
    base = pe + 24 + opt_size
    out = []
    for i in range(count):
        off = base + i * 40
        if off + 40 > len(data):
            raise ValueError("truncated section table")
        vsize, vaddr, rsize, rptr = struct.unpack_from("<IIII", data, off + 8)
        out.append((vaddr, max(vsize, rsize), rptr))
    return out


def pe_imported_dlls(data: bytes) -> list:
    """Return DLL names from the import and delay-load import directories."""
    if len(data) < 0x40:
        raise ValueError("file too small for a PE header")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("missing PE signature")
    _machine, nsec, _ts, _sym, _nsym, opt_size, _chars = struct.unpack_from(
        "<HHIIIHH", data, pe + 4
    )
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    if magic == 0x20B:  # PE32+
        dd_off = opt + 112
    elif magic == 0x10B:  # PE32
        dd_off = opt + 96
    else:
        raise ValueError("unknown optional header magic")
    nrva = struct.unpack_from("<I", data, dd_off - 4)[0]
    sections = _sections(data, pe, nsec, opt_size, 0)

    def rva_to_off(rva):
        for vaddr, size, rptr in sections:
            if vaddr <= rva < vaddr + size:
                return rptr + (rva - vaddr)
        raise ValueError(f"RVA 0x{rva:x} not in any section")

    def cstr(rva):
        off = rva_to_off(rva)
        end = data.index(b"\0", off)
        return data[off:end].decode("ascii", "replace")

    def directory(index):
        if index >= nrva:
            return 0
        return struct.unpack_from("<II", data, dd_off + index * 8)[0]

    names = []
    imp = directory(1)  # IMAGE_DIRECTORY_ENTRY_IMPORT
    if imp:
        off = rva_to_off(imp)
        while True:
            fields = struct.unpack_from("<IIIII", data, off)
            if fields == (0, 0, 0, 0, 0):
                break
            names.append(cstr(fields[3]))  # Name RVA
            off += 20
    delay = directory(13)  # IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT
    if delay:
        off = rva_to_off(delay)
        while True:
            attrs, name_rva = struct.unpack_from("<II", data, off)
            if attrs == 0 and name_rva == 0:
                break
            # attrs bit 0 set => RVAs (modern); clear => legacy VAs, not supported.
            if attrs & 1:
                names.append(cstr(name_rva))
            off += 32
    return names


def check_pe(path: Path, data: bytes) -> list:
    try:
        names = pe_imported_dlls(data)
    except (ValueError, struct.error) as error:
        return [f"{path}: cannot parse PE imports: {error}"]
    return [
        f"{path}: imports OpenSSL DLL {n} (must be statically vendored)"
        for n in names if OPENSSL_DLL.match(n)
    ]


def check_binary(path: Path) -> list:
    with open(path, "rb") as handle:
        data = handle.read()
    kind = detect_format(data)
    if kind == "macho":
        return check_macho(path)
    if kind == "pe":
        return check_pe(path, data)
    if kind == "elf":
        print(f"NOTE: {path}: ELF — covered by check_glibc.py / static vendoring")
        return []
    return [f"{path}: unrecognised binary format"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binaries", type=Path, nargs="+")
    args = parser.parse_args()
    errors = []
    for path in args.binaries:
        try:
            errors.extend(check_binary(path))
        except OSError as error:
            errors.append(f"{path}: {error}")
    for message in errors:
        print(f"ERROR: {message}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
