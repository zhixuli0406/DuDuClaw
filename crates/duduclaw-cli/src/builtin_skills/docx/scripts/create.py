# /// script
# requires-python = ">=3.9"
# dependencies = ["python-docx>=1.1"]
# ///
"""Create a .docx from a markdown or JSON source file.

Usage:
    uv run create.py report.md  --out /abs/out.docx    # markdown source
    uv run create.py spec.json  --out /abs/out.docx    # JSON source

The source kind is inferred from the input extension (`.json` → JSON, anything
else → markdown), or forced with `--format md|json`.

Markdown subset: `#`/`##`/`###` headings, `- ` bullet lists, `| a | b |`
tables, blank-line-separated paragraphs.

JSON schema: {"title": str?, "blocks": [
    {"type": "heading", "level": 1..3, "text": str},
    {"type": "paragraph", "text": str},
    {"type": "bullet", "text": str} | {"type": "bullets", "items": [str, ...]},
    {"type": "table", "rows": [[str, ...], ...]}
]}
(This is the same schema `extract.py --format json` emits, so extract → create
round-trips.)
"""
import argparse
import json
import sys

try:
    from docx import Document
except ImportError:
    sys.stderr.write(
        "python-docx not installed. Run via `uv run` or `pip install python-docx`.\n"
    )
    sys.exit(2)


def add_table(doc, rows):
    if not rows:
        return
    ncols = max(len(r) for r in rows)
    table = doc.add_table(rows=0, cols=ncols)
    table.style = "Table Grid"
    for r in rows:
        cells = table.add_row().cells
        for i in range(ncols):
            cells[i].text = r[i] if i < len(r) else ""


def build_from_json(doc, spec):
    if spec.get("title"):
        doc.add_heading(spec["title"], level=0)
    for block in spec.get("blocks", []):
        t = block.get("type")
        if t == "heading":
            doc.add_heading(block.get("text", ""), level=int(block.get("level", 1)))
        elif t == "paragraph":
            doc.add_paragraph(block.get("text", ""))
        elif t == "bullet":
            doc.add_paragraph(block.get("text", ""), style="List Bullet")
        elif t == "bullets":
            for item in block.get("items", []):
                doc.add_paragraph(str(item), style="List Bullet")
        elif t == "table":
            add_table(doc, block.get("rows", []))


def parse_md_table(lines, start):
    """Consume a contiguous run of `|`-delimited lines starting at `start`."""
    rows = []
    i = start
    while i < len(lines) and lines[i].lstrip().startswith("|"):
        cells = [c.strip() for c in lines[i].strip().strip("|").split("|")]
        # skip separator rows like |---|---|
        if not all(set(c) <= set("-: ") and c for c in cells):
            rows.append(cells)
        i += 1
    return rows, i


def build_from_md(doc, text):
    lines = text.splitlines()
    i = 0
    bullets = []

    def flush_bullets():
        for b in bullets:
            doc.add_paragraph(b, style="List Bullet")
        bullets.clear()

    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if stripped.startswith("|"):
            flush_bullets()
            rows, i = parse_md_table(lines, i)
            add_table(doc, rows)
            continue
        if stripped.startswith("### "):
            flush_bullets()
            doc.add_heading(stripped[4:], level=3)
        elif stripped.startswith("## "):
            flush_bullets()
            doc.add_heading(stripped[3:], level=2)
        elif stripped.startswith("# "):
            flush_bullets()
            doc.add_heading(stripped[2:], level=1)
        elif stripped.startswith("- ") or stripped.startswith("* "):
            bullets.append(stripped[2:])
        elif stripped:
            flush_bullets()
            doc.add_paragraph(stripped)
        else:
            flush_bullets()
        i += 1
    flush_bullets()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--out", required=True)
    ap.add_argument("--format", choices=["md", "json"], default=None,
                    help="source kind; inferred from the input extension if omitted")
    args = ap.parse_args()

    kind = args.format or ("json" if args.input.lower().endswith(".json") else "md")
    doc = Document()
    try:
        with open(args.input, encoding="utf-8") as f:
            if kind == "json":
                build_from_json(doc, json.load(f))
            else:
                build_from_md(doc, f.read())
        doc.save(args.out)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"Failed to create docx: {e}\n")
        sys.exit(1)

    print(args.out)


if __name__ == "__main__":
    main()
