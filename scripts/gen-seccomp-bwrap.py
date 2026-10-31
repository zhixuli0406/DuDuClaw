#!/usr/bin/env python3
"""Generate the bubblewrap profile from the pinned Moby default (stdlib only).

Source commit: 4b8214fc4971e06172938a1f1554ccff6647ee80
Source SHA-256: 6416b47770785a41ac59073cdc77d9fe98517df2799dc83ef207e622de3053f6
Output is written to stdout; --input avoids all network access.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sys
import urllib.request

COMMIT = "4b8214fc4971e06172938a1f1554ccff6647ee80"
SHA256 = "6416b47770785a41ac59073cdc77d9fe98517df2799dc83ef207e622de3053f6"
URL = f"https://raw.githubusercontent.com/moby/profiles/{COMMIT}/seccomp/default.json"
REMOVED = frozenset(("clone", "clone3", "mount", "umount2", "unshare"))
ALLOWED = ["clone", "clone3", "mount", "umount2", "pivot_root", "unshare"]


def generate(raw):
    digest = hashlib.sha256(raw).hexdigest()
    if digest != SHA256:
        raise ValueError(f"source SHA-256 mismatch: expected {SHA256}, got {digest}")
    profile = json.loads(raw)
    rules = []
    for rule in profile["syscalls"]:
        rule["names"] = [name for name in rule["names"] if name not in REMOVED]
        if rule["names"]:
            rules.append(rule)
    rules.append({"names": ALLOWED, "action": "SCMP_ACT_ALLOW"})
    profile["syscalls"] = rules
    return json.dumps(profile, indent=2) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, help="read the pinned source locally")
    args = parser.parse_args()
    try:
        if args.input is not None:
            raw = args.input.read_bytes()
        else:
            with urllib.request.urlopen(URL, timeout=30) as response:
                raw = response.read()
        result = generate(raw)
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")
    sys.stdout.write(result)


if __name__ == "__main__":
    main()
