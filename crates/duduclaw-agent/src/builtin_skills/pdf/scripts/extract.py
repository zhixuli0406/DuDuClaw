# /// script
# requires-python = ">=3.9"
# dependencies = ["pypdf>=4.0"]
# ///
"""Extract text from a PDF, page by page.

Usage:
    uv run extract.py <input.pdf> [--format json|md]

JSON: {"pages": ["page 1 text", "page 2 text", ...]}.
md:   page texts joined with `\n\n---\n\n` separators.
"""
import argparse
import json
import os
import sys

try:
    from pypdf import PdfReader
except ImportError:
    sys.stderr.write("pypdf not installed. Run via `uv run` or `pip install pypdf`.\n")
    sys.exit(2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--format", choices=["json", "md"], default="json")
    args = ap.parse_args()

    if not os.path.exists(args.input):
        sys.stderr.write(f"Input not found: {args.input}\n")
        sys.exit(1)

    try:
        reader = PdfReader(args.input)
        pages = [(p.extract_text() or "").strip() for p in reader.pages]
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"Failed to read {args.input}: {e}\n")
        sys.exit(1)

    if args.format == "json":
        print(json.dumps({"pages": pages}, ensure_ascii=False, indent=2))
    else:
        print("\n\n---\n\n".join(pages))


if __name__ == "__main__":
    main()
