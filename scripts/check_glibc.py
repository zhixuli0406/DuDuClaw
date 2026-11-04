#!/usr/bin/env python3
"""Fail closed when an ELF binary requires glibc newer than our 2.31 baseline."""

import os
import re
import shutil
import subprocess
import sys

BASELINE = (2, 31, 0)
GLIBC_VERSION_RE = re.compile(rb'GLIBC_(\d+)\.(\d+)(?:\.(\d+))?')


def check_versions(version_info: str) -> tuple[int, ...]:
    versions = [tuple(map(int, match.split('.'))) for match in
                re.findall(r'\bGLIBC_(\d+(?:\.\d+)+)\b', version_info)]
    if not versions:
        raise ValueError('No required GLIBC symbol versions found')
    maximum = max(version + (0,) * (3 - len(version)) for version in versions)
    if maximum > BASELINE:
        raise ValueError(f'Requires GLIBC_{format_version(maximum)}; maximum allowed is GLIBC_2.31')
    return maximum


def check_elf_bytes(binary: bytes) -> tuple[int, ...]:
    if not binary.startswith(b'\x7fELF'):
        raise ValueError('File is not an ELF binary')
    versions = [tuple(int(part) for part in match.groups() if part is not None)
                for match in GLIBC_VERSION_RE.finditer(binary)]
    if not versions:
        raise ValueError('No required GLIBC symbol versions found')
    maximum = max(version + (0,) * (3 - len(version)) for version in versions)
    if maximum > BASELINE:
        raise ValueError(f'Requires GLIBC_{format_version(maximum)}; maximum allowed is GLIBC_2.31')
    return maximum


def format_version(version: tuple[int, ...]) -> str:
    return '.'.join(map(str, version[:2] if version[2:] == (0,) else version))


def main() -> int:
    if len(sys.argv) != 2:
        print('Usage: python3 scripts/check_glibc.py <ELF binary>', file=sys.stderr)
        return 2
    try:
        binary_path = sys.argv[1]
        readelf_override = os.environ.get('READELF')
        readelf = readelf_override or shutil.which('readelf')
        if readelf:
            result = subprocess.run([readelf, '--version-info', '--', binary_path],
                                    check=True, capture_output=True, text=True)
            maximum = check_versions(result.stdout)
            method = 'readelf'
        else:
            with open(binary_path, 'rb') as binary_file:
                maximum = check_elf_bytes(binary_file.read())
            method = 'Python ELF byte scan'
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f'glibc baseline check FAILED: {error}', file=sys.stderr)
        return 1
    print(f'{binary_path}: {method} found maximum required GLIBC_{format_version(maximum)} <= GLIBC_2.31')
    return 0


if __name__ == '__main__':
    sys.exit(main())
