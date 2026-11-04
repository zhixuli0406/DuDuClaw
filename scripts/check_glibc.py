#!/usr/bin/env python3
"""Fail closed when an ELF binary requires glibc newer than our 2.31 baseline."""

import re
import subprocess
import sys

BASELINE = (2, 31, 0)


def check_versions(version_info: str) -> tuple[int, ...]:
    versions = [tuple(map(int, match.split('.'))) for match in
                re.findall(r'\bGLIBC_(\d+(?:\.\d+)+)\b', version_info)]
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
        result = subprocess.run(['readelf', '--version-info', '--', sys.argv[1]],
                                check=True, capture_output=True, text=True)
        maximum = check_versions(result.stdout)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f'glibc baseline check FAILED: {error}', file=sys.stderr)
        return 1
    print(f'{sys.argv[1]}: maximum required GLIBC_{format_version(maximum)} <= GLIBC_2.31')
    return 0


if __name__ == '__main__':
    sys.exit(main())
