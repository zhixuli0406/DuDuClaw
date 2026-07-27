# /// script
# requires-python = ">=3.9"
# dependencies = ["reportlab>=4.0"]
# ///
"""Render a markdown or plain-text source file into a PDF.

Usage:
    uv run create.py report.md  --out /abs/out.pdf   # markdown source
    uv run create.py notes.txt  --out /abs/out.pdf   # plain-text source

The source is parsed as markdown by default; `.txt` inputs (or `--format text`)
skip heading/bullet interpretation. Markdown subset: `#`/`##`/`###` headings
(larger font), `- ` bullets, blank-line-separated paragraphs. Text wraps to the
page width. Uses reportlab's built-in fonts; a CJK font (STSong-Light) is
registered when available so Chinese renders instead of tofu.
"""
import argparse
import sys

try:
    from reportlab.lib.pagesizes import A4
    from reportlab.lib.styles import getSampleStyleSheet, ParagraphStyle
    from reportlab.lib.units import cm
    from reportlab.platypus import SimpleDocTemplate, Paragraph, Spacer
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.cidfonts import UnicodeCIDFont
except ImportError:
    sys.stderr.write(
        "reportlab not installed. Run via `uv run` or `pip install reportlab`.\n"
    )
    sys.exit(2)


def register_cjk():
    try:
        pdfmetrics.registerFont(UnicodeCIDFont("STSong-Light"))
        return "STSong-Light"
    except Exception:  # noqa: BLE001
        return None


def esc(text):
    return (
        text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    )


def build_flowables(text, styles, markdown=True):
    flow = []
    for raw in text.splitlines():
        line = raw.rstrip()
        stripped = line.strip()
        if not stripped:
            flow.append(Spacer(1, 0.3 * cm))
            continue
        if not markdown:
            # Plain text: no heading/bullet interpretation.
            flow.append(Paragraph(esc(stripped), styles["body"]))
            continue
        if stripped.startswith("### "):
            flow.append(Paragraph(esc(stripped[4:]), styles["h3"]))
        elif stripped.startswith("## "):
            flow.append(Paragraph(esc(stripped[3:]), styles["h2"]))
        elif stripped.startswith("# "):
            flow.append(Paragraph(esc(stripped[2:]), styles["h1"]))
        elif stripped.startswith("- ") or stripped.startswith("* "):
            flow.append(Paragraph("• " + esc(stripped[2:]), styles["body"]))
        else:
            flow.append(Paragraph(esc(stripped), styles["body"]))
    return flow


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--out", required=True)
    ap.add_argument("--format", choices=["md", "text"], default=None,
                    help="source kind; `.txt` inputs default to text, else markdown")
    args = ap.parse_args()

    kind = args.format or ("text" if args.input.lower().endswith(".txt") else "md")
    try:
        with open(args.input, encoding="utf-8") as f:
            text = f.read()
    except OSError as e:
        sys.stderr.write(f"Failed to read {args.input}: {e}\n")
        sys.exit(1)

    font = register_cjk()
    base = getSampleStyleSheet()
    fam = font or "Helvetica"
    styles = {
        "h1": ParagraphStyle("h1", parent=base["Heading1"], fontName=fam),
        "h2": ParagraphStyle("h2", parent=base["Heading2"], fontName=fam),
        "h3": ParagraphStyle("h3", parent=base["Heading3"], fontName=fam),
        "body": ParagraphStyle("body", parent=base["BodyText"], fontName=fam, leading=16),
    }

    try:
        doc = SimpleDocTemplate(args.out, pagesize=A4)
        doc.build(build_flowables(text, styles, markdown=(kind == "md")))
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"Failed to create pdf: {e}\n")
        sys.exit(1)

    print(args.out)


if __name__ == "__main__":
    main()
