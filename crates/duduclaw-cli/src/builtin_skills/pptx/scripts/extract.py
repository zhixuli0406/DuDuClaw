# /// script
# requires-python = ">=3.9"
# dependencies = ["python-pptx>=0.6.23"]
# ///
"""Extract slide text from a .pptx file.

Usage:
    uv run extract.py <input.pptx> [--format json|md]

JSON: {"slides": [{"index": 1, "texts": ["...", ...]}, ...]}.
"""
import argparse
import json
import os
import sys

try:
    from pptx import Presentation
except ImportError:
    sys.stderr.write(
        "python-pptx not installed. Run via `uv run` or `pip install python-pptx`.\n"
    )
    sys.exit(2)


def slide_texts(slide):
    texts = []
    for shape in slide.shapes:
        if shape.has_text_frame:
            for para in shape.text_frame.paragraphs:
                line = "".join(run.text for run in para.runs).strip()
                if line:
                    texts.append(line)
    return texts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--format", choices=["json", "md"], default="json")
    args = ap.parse_args()

    if not os.path.exists(args.input):
        sys.stderr.write(f"Input not found: {args.input}\n")
        sys.exit(1)

    try:
        prs = Presentation(args.input)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"Failed to open {args.input}: {e}\n")
        sys.exit(1)

    slides = [
        {"index": i, "texts": slide_texts(s)}
        for i, s in enumerate(prs.slides, start=1)
    ]

    if args.format == "json":
        print(json.dumps({"slides": slides}, ensure_ascii=False, indent=2))
    else:
        parts = []
        for s in slides:
            head = s["texts"][0] if s["texts"] else f"Slide {s['index']}"
            body = "\n".join(f"- {t}" for t in s["texts"][1:])
            parts.append(f"## {head}\n{body}".rstrip())
        print("\n\n".join(parts))


if __name__ == "__main__":
    main()
