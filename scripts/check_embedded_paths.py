#!/usr/bin/env python3
"""Check literal Rust embedded paths against a Docker build stage's COPY sources.

Usage: python3 scripts/check_embedded_paths.py . container/Dockerfile.server [--stage NAME]
"""

import argparse
import os
from pathlib import Path
import re
import shlex
import sys

INCLUDE = re.compile(
    r'\binclude_(?:str|bytes)!\s*\(\s*'
    r'(?:r(?P<hashes>\#*)"(?P<raw>[^"]*)"(?P=hashes)|"(?P<plain>(?:\\.|[^"\\])*)")'
    r'\s*,?\s*\)'
)
# Preserve strings and line numbers while masking comments before scanning.
RUST_TOKEN = re.compile(
    r'(?P<comment>//[^\n]*|/\*.*?\*/)|r(?P<hashes>\#*)".*?"(?P=hashes)|"(?:\\.|[^"\\])*"',
    re.DOTALL,
)


def decode_path(literal):
    def unescape(match):
        value = match.group(1)
        if value.startswith("x"):
            return chr(int(value[1:], 16))
        if value.startswith("u{"):
            return chr(int(value[2:-1].replace("_", ""), 16))
        escapes = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", '"': '"', "'": "'", "\\": "\\"}
        if value not in escapes:
            raise ValueError(f"Unsupported escape in include path: {literal}")
        return escapes[value]
    return re.sub(r"\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)", unescape, literal)


def copied_paths(root, dockerfile, stage):
    covered = []
    in_builder = False
    stage_found = False
    text = re.sub(r"\\\r?\n", " ", dockerfile.read_text(encoding="utf-8"))
    for line in text.splitlines():
        words = shlex.split(line, comments=True)
        if not words:
            continue
        instruction = words[0].upper()
        if instruction == "FROM":
            in_builder = len(words) >= 4 and words[-2].upper() == "AS" and words[-1].lower() == stage.lower()
            stage_found = stage_found or in_builder
        elif in_builder and instruction == "COPY":
            if any(word.startswith("--from=") for word in words[1:]):
                continue
            sources = [word for word in words[1:-1] if not word.startswith("--")]
            for source in sources:
                path = (root / source).resolve()
                covered.append((path, source.endswith("/") or path.is_dir()))
    if not stage_found:
        raise ValueError(f"Docker stage not found: {stage}")
    if not covered:
        raise ValueError(f"No COPY sources found in {stage} stage")
    return covered


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo_root", type=Path)
    parser.add_argument("dockerfile", type=Path)
    parser.add_argument("--stage", default="rust-builder")
    args = parser.parse_args(argv)
    root = args.repo_root.resolve()
    crates = root / "crates"
    try:
        if not crates.is_dir():
            raise ValueError(f"Missing Rust source directory: {crates}")
        covered = copied_paths(root, args.dockerfile, args.stage)
        missing = []
        for source in sorted(crates.rglob("*.rs")):
            relative = source.relative_to(root)
            if ("tests" in relative.parts or source.name.endswith("_tests.rs")
                    or source.name == "tests.rs" or source.name.startswith("tests_")):
                continue
            text = source.read_text(encoding="utf-8")
            text = RUST_TOKEN.sub(
                lambda token: re.sub(r"[^\n]", " ", token.group())
                if token.group("comment") is not None else token.group(), text,
            )
            for match in INCLUDE.finditer(text):
                literal = decode_path(match.group("plain")) if match.group("plain") is not None else match.group("raw")
                if Path(literal).is_absolute():
                    continue
                target = (source.parent / literal).resolve()
                if target.is_relative_to(crates):
                    continue
                if any(target == path or (is_dir and target.is_relative_to(path))
                       for path, is_dir in covered):
                    continue
                line = text.count("\n", 0, match.start()) + 1
                missing.append(f"{relative}:{line} -> {os.path.relpath(target, root)}")
        if missing:
            print(f"Embedded path check FAILED: add a COPY line to the {args.stage} stage for each target:")
            print("\n".join(missing))
            return 1
    except (OSError, UnicodeError, ValueError) as error:
        print(f"Embedded path check FAILED: {error}")
        return 1
    print(f"Embedded path check OK: all external literal includes are covered by {args.stage} COPY sources.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
