# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""Convert an office document to PDF via LibreOffice headless.

Usage:
    uv run to_pdf.py <input> [--outdir DIR]

Graceful degrade: if `soffice`/`libreoffice` is not installed, prints a clear
message and exits non-zero. Read/create features of the sibling scripts are
unaffected — only conversion needs LibreOffice.
"""
import argparse
import os
import shutil
import subprocess
import sys


def find_soffice():
    for name in ("soffice", "libreoffice"):
        path = shutil.which(name)
        if path:
            return path
    # Common macOS install location not always on PATH.
    mac = "/Applications/LibreOffice.app/Contents/MacOS/soffice"
    if os.path.exists(mac):
        return mac
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("--outdir", default=None)
    args = ap.parse_args()

    if not os.path.exists(args.input):
        sys.stderr.write(f"Input not found: {args.input}\n")
        sys.exit(1)

    soffice = find_soffice()
    if not soffice:
        sys.stderr.write(
            "LibreOffice (soffice) 未安裝，僅『轉換 PDF』功能不可用；讀取與建立功能不受影響。\n"
            "安裝方式：macOS `brew install --cask libreoffice`；"
            "Debian/Ubuntu `apt-get install libreoffice`.\n"
        )
        sys.exit(3)

    outdir = args.outdir or os.path.dirname(os.path.abspath(args.input))
    os.makedirs(outdir, exist_ok=True)
    try:
        subprocess.run(
            [soffice, "--headless", "--convert-to", "pdf", "--outdir", outdir, args.input],
            check=True,
            capture_output=True,
            timeout=180,
        )
    except subprocess.CalledProcessError as e:  # noqa: PERF203
        sys.stderr.write(f"Conversion failed: {e.stderr.decode(errors='replace')}\n")
        sys.exit(1)
    except subprocess.TimeoutExpired:
        sys.stderr.write("Conversion timed out after 180s.\n")
        sys.exit(1)

    base = os.path.splitext(os.path.basename(args.input))[0]
    out_pdf = os.path.join(outdir, base + ".pdf")
    if not os.path.exists(out_pdf):
        sys.stderr.write("Conversion reported success but no PDF was produced.\n")
        sys.exit(1)
    print(out_pdf)


if __name__ == "__main__":
    main()
