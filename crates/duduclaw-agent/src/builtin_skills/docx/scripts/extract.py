# /// script
# requires-python = ">=3.9"
# dependencies = ["python-docx>=1.1"]
# ///
"""Extract text and tables from a .docx file, in document order.

Usage:
    uv run extract.py <input.docx> [--format json|md]

`json` gives {"blocks": [{"type": "paragraph"|"heading"|"table", ...}]} in the
same order they appear in the document body (paragraphs, headings, and tables
interleaved — never regrouped). `md` renders those blocks to readable markdown.
"""
import argparse
import json
import sys

try:
    from docx import Document
    from docx.table import Table
    from docx.text.paragraph import Paragraph
except ImportError:
    sys.stderr.write(
        "python-docx not installed. Run via `uv run` (auto-installs) or "
        "`pip install python-docx`.\n"
    )
    sys.exit(2)


def table_rows(table):
    return [[cell.text for cell in row.cells] for row in table.rows]


def iter_body_blocks(doc):
    """Yield Paragraph / Table objects in true document-body order.

    `Document.iter_inner_content()` (python-docx >= 1.0) walks the body in
    order; fall back to manually zipping the body XML against the paragraph /
    table maps on older builds so ordering is preserved either way.
    """
    if hasattr(doc, "iter_inner_content"):
        yield from doc.iter_inner_content()
        return
    parent_elm = doc.element.body
    para_map = {p._p: p for p in doc.paragraphs}
    table_map = {t._tbl: t for t in doc.tables}
    for child in parent_elm.iterchildren():
        if child in para_map:
            yield para_map[child]
        elif child in table_map:
            yield table_map[child]


def block_kind(paragraph):
    style = (paragraph.style.name or "") if paragraph.style else ""
    if style.startswith("Heading"):
        level = "".join(ch for ch in style if ch.isdigit())
        return "heading", int(level) if level else 1
    if "List" in style:
        return "bullet", 0
    return "paragraph", 0


def to_blocks(doc):
    blocks = []
    for item in iter_body_blocks(doc):
        if isinstance(item, Table):
            blocks.append({"type": "table", "rows": table_rows(item)})
        elif isinstance(item, Paragraph):
            text = item.text.strip()
            if not text:
                continue
            kind, level = block_kind(item)
            if kind == "heading":
                blocks.append({"type": "heading", "level": level, "text": text})
            elif kind == "bullet":
                blocks.append({"type": "bullet", "text": text})
            else:
                blocks.append({"type": "paragraph", "text": text})
    return blocks


def md_table(rows):
    if not rows:
        return ""
    header = rows[0]
    out = ["| " + " | ".join(header) + " |",
           "| " + " | ".join("---" for _ in header) + " |"]
    for r in rows[1:]:
        cells = (r + [""] * len(header))[: len(header)]
        out.append("| " + " | ".join(cells) + " |")
    return "\n".join(out)


def blocks_to_md(blocks):
    out = []
    for b in blocks:
        t = b["type"]
        if t == "heading":
            out.append("#" * max(1, min(6, b.get("level", 1))) + " " + b["text"])
        elif t == "bullet":
            out.append("- " + b["text"])
        elif t == "table":
            out.append(md_table(b["rows"]))
        else:
            out.append(b["text"])
    return "\n\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--format", choices=["json", "md"], default="json")
    args = ap.parse_args()

    try:
        doc = Document(args.input)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"Failed to open {args.input}: {e}\n")
        sys.exit(1)

    blocks = to_blocks(doc)
    if args.format == "json":
        print(json.dumps({"blocks": blocks}, ensure_ascii=False, indent=2))
    else:
        print(blocks_to_md(blocks))


if __name__ == "__main__":
    main()
