# /// script
# requires-python = ">=3.9"
# dependencies = ["python-pptx>=0.6.23"]
# ///
"""Create a .pptx from a markdown outline or JSON source file.

Usage:
    uv run create.py outline.md --out /abs/out.pptx   # markdown outline
    uv run create.py deck.json  --out /abs/out.pptx   # JSON source

The source kind is inferred from the input extension (`.json` → JSON, else
markdown), or forced with `--format md|json`.

Markdown: each `#`/`##` heading starts a new slide; `- ` lines become bullets.
JSON schema: {"slides": [{"title": str, "bullets": [str, ...]}, ...]}.
"""
import argparse
import json
import sys

try:
    from pptx import Presentation
    from pptx.util import Pt
except ImportError:
    sys.stderr.write(
        "python-pptx not installed. Run via `uv run` or `pip install python-pptx`.\n"
    )
    sys.exit(2)


def add_slide(prs, title, bullets):
    layout = prs.slide_layouts[1]  # Title and Content
    slide = prs.slides.add_slide(layout)
    slide.shapes.title.text = title or ""
    body = slide.placeholders[1].text_frame
    body.clear()
    for i, b in enumerate(bullets):
        para = body.paragraphs[0] if i == 0 else body.add_paragraph()
        para.text = str(b)
        para.font.size = Pt(18)


def slides_from_md(text):
    slides = []
    cur = None
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("#"):
            if cur:
                slides.append(cur)
            cur = {"title": line.lstrip("#").strip(), "bullets": []}
        elif line.startswith("- ") or line.startswith("* "):
            if cur is None:
                cur = {"title": "", "bullets": []}
            cur["bullets"].append(line[2:])
        elif line and cur is not None:
            cur["bullets"].append(line)
    if cur:
        slides.append(cur)
    return slides


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--out", required=True)
    ap.add_argument("--format", choices=["md", "json"], default=None,
                    help="source kind; inferred from the input extension if omitted")
    args = ap.parse_args()

    kind = args.format or ("json" if args.input.lower().endswith(".json") else "md")
    try:
        if kind == "json":
            with open(args.input, encoding="utf-8") as f:
                slides = json.load(f).get("slides", [])
        else:
            with open(args.input, encoding="utf-8") as f:
                slides = slides_from_md(f.read())
        prs = Presentation()
        for s in slides:
            add_slide(prs, s.get("title", ""), s.get("bullets", []))
        prs.save(args.out)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"Failed to create pptx: {e}\n")
        sys.exit(1)

    print(args.out)


if __name__ == "__main__":
    main()
