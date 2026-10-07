#!/usr/bin/env python3
"""Build a versioned distributor brief without changing its factual baseline."""

import argparse
import html
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


SOURCE_FILES = ("build.py", "page-body.html", "style.css.part", "extra.css", "enable_data.py")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
DEFAULT_SOURCE = Path(__file__).resolve().parent.parent / "commercial/marketing/distributor/technology-brief-v1.67-src"


def render(source, version):
    for name in SOURCE_FILES:
        if not (source / name).is_file():
            raise ValueError(f"Missing source file: {source / name}")
    body = (source / "page-body.html").read_text(encoding="utf-8")
    baseline = re.search(r"產品版本\s+v([^\s<（(]+)", body)
    if not baseline:
        raise ValueError("Cannot find the product version in page-body.html")
    baseline = baseline.group(1)
    if not VERSION.fullmatch(baseline):
        raise ValueError("Invalid product version in page-body.html")
    with tempfile.TemporaryDirectory(prefix="duduclaw-brief-build-") as temporary:
        staging = Path(temporary)
        for name in SOURCE_FILES:
            shutil.copyfile(source / name, staging / name)
        subprocess.run([sys.executable, "build.py"], cwd=staging, check=True)
        fragment = (staging / "duduclaw-tech-brief.html").read_text(encoding="utf-8")
    fragment, removed = re.subn(r"\A\s*<title\b[^>]*>.*?</title>\s*", "", fragment, count=1, flags=re.S | re.I)
    if removed != 1 or re.search(r"<title\b|<!doctype\b|</?(?:html|head|body)\b", fragment, re.I):
        raise ValueError("Builder must produce a fragment with one leading title")
    styles = []
    while match := re.match(r"\s*(<style\b[^>]*>.*?</style>)\s*", fragment, re.S | re.I):
        styles.append(match.group(1))
        fragment = fragment[match.end():]
    if version != baseline:
        notice = (f"發版文件 v{version}；內容查核基準 v{baseline}，功能說明尚待逐項確認。")
        # Limit the insertion to the header metadata; preserve all historical claims.
        header = re.search(r"<header\b[^>]*>.*?</header>", fragment, re.S | re.I)
        if not header:
            raise ValueError("Cannot find the header for the content baseline notice")
        updated, count = re.subn(r'(<div\b[^>]*\bclass=[\"\']meta[\"\'][^>]*>)',
                                 lambda m: m.group(1) + '<span class="release-baseline">' + html.escape(notice) + '</span>',
                                 header.group(), count=1, flags=re.I)
        if count != 1:
            raise ValueError("Cannot find header metadata for the content baseline notice")
        fragment = fragment[:header.start()] + updated + fragment[header.end():]
    style = "\n".join(styles)
    return (f'<!DOCTYPE html>\n<html lang="zh-Hant-TW">\n<head>\n'
            f'<meta charset="utf-8">\n<meta name="viewport" content="width=device-width, initial-scale=1">\n'
            f'<title>DuDuClaw 技術特色說明 v{version}</title>\n{style}\n</head>\n<body>\n'
            f'{fragment.rstrip()}\n</body>\n</html>\n').encode("utf-8")


def publish(output, content, force):
    if output.exists():
        if output.read_bytes() == content:
            return
        if not force:
            raise ValueError(f"Refusing to overwrite different output: {output}; use --force to rebuild")
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{output.name}.", dir=output.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(content)
        temporary.chmod(0o644)
        if force:
            os.replace(temporary, output)
        else:
            # Exclusive installation also refuses another process's concurrent output.
            os.link(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="Release version, e.g. 1.69.1 (without v)")
    parser.add_argument("--source-dir", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--skip-missing", action="store_true", help="Allow an absent private source directory")
    parser.add_argument("--force", action="store_true", help="Replace a different existing file for this version")
    args = parser.parse_args()
    if not VERSION.fullmatch(args.version):
        parser.error("--version must be a numeric X.Y.Z version without a prefix")
    source = args.source_dir.resolve()
    if not source.exists() and args.skip_missing:
        print(f"SKIP: private distributor source directory is absent: {source}")
        return 0
    try:
        if not source.is_dir():
            raise ValueError(f"Source directory does not exist: {source}")
        content = render(source, args.version)
        output_dir = args.output_dir.resolve() if args.output_dir else source.parent
        output = output_dir / f"duduclaw-technology-brief-v{args.version}.html"
        publish(output, content, args.force)
        print(f"Distributor brief: {output}")
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
