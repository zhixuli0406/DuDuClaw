"""Keep retried passes and failures visible in the Actions step summary."""

import html
import os
from pathlib import Path
import sys
import xml.etree.ElementTree as ET


def summarize(path: Path) -> str:
    title = f"### nextest: {html.escape(path.name)}\n\n"
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as error:
        return title + f"JUnit report unavailable: {html.escape(str(error))}\n\n"
    lines = []
    for case in root.iter("testcase"):
        tags = {child.tag for child in case}
        name = html.escape(f"{case.get('classname', '')}::{case.get('name', '')}")
        if tags & {"flakyFailure", "flakyError"}:
            lines.append(f"- **FLAKY** <code>{name}</code>\n")
        if tags & {"failure", "error"}:
            lines.append(f"- **FAILED** <code>{name}</code>\n")
    return title + ("".join(lines) or "No flaky or failed tests.\n") + "\n"


def main() -> None:
    # Move each report immediately after its suite, before nextest overwrites it.
    source, destination = map(Path, sys.argv[1:3])
    if source.is_file():
        destination.parent.mkdir(parents=True, exist_ok=True)
        source.replace(destination)
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as stream:
        stream.write(summarize(destination))


if __name__ == "__main__":
    main()
