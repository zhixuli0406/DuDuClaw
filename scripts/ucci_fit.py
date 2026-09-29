#!/usr/bin/env python3
"""Fit a UCCI router only from paired, human-reviewed DuDuClaw examples.

Install `ucci-router` first. Input is JSONL with one row per query and stage:
id, stage, u, small_answer, large_answer, small_correct, large_correct,
label_source="human", and optionally split="cal"|"val"|"test".
The chosen accuracy target and measured per-call costs are explicit inputs.
"""

import argparse
import json
import math
import subprocess
import sys
from pathlib import Path


def load_reviewed(path: Path, stage: str) -> list[dict]:
    rows = []
    ids = set()
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("stage") != stage:
            continue
        identity = row.get("id")
        if not isinstance(identity, str) or not identity or identity in ids:
            raise ValueError(f"line {line_number}: id must be unique and nonempty")
        ids.add(identity)
        if row.get("label_source") != "human":
            raise ValueError(f"line {line_number}: label_source must be 'human'")
        small_answer = row.get("small_answer", row.get("answer"))
        if not isinstance(small_answer, str) or not small_answer.strip() \
                or not isinstance(row.get("large_answer"), str) or not row["large_answer"].strip():
            raise ValueError(f"line {line_number}: both reviewed answers are required")
        signal = row.get("u")
        if isinstance(signal, bool) or not isinstance(signal, (int, float)) \
                or not math.isfinite(signal) or not 0 <= signal <= 1:
            raise ValueError(f"line {line_number}: u must be finite and within [0, 1]")
        for key in ("small_correct", "large_correct"):
            if type(row.get(key)) not in (bool, int) or row[key] not in (0, 1):
                raise ValueError(f"line {line_number}: {key} must be 0 or 1")
        rows.append({key: row[key] for key in ("id", "u", "small_correct", "large_correct")})
        if "split" in row:
            if row["split"] not in ("cal", "val", "test"):
                raise ValueError(f"line {line_number}: split must be cal, val or test")
            rows[-1]["split"] = row["split"]
    if not rows:
        raise ValueError(f"no reviewed rows for stage {stage}")
    if any("split" in row for row in rows) and not all("split" in row for row in rows):
        raise ValueError("either every row has a split or none does")
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", required=True, type=Path)
    parser.add_argument("--stage", required=True, choices=("local_fast", "local_strong"))
    parser.add_argument("--tau", required=True, type=float, help="validation accuracy target")
    parser.add_argument("--c-small", required=True, type=float, help="measured local call cost")
    parser.add_argument("--c-large", required=True, type=float, help="measured escalation call cost")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if not math.isfinite(args.tau) or not 0 <= args.tau <= 1:
        parser.error("--tau must be within [0, 1]")
    if not all(math.isfinite(v) and v > 0 for v in (args.c_small, args.c_large)):
        parser.error("costs must be finite and positive")
    try:
        rows = load_reviewed(args.data, args.stage)
    except (ValueError, OSError, json.JSONDecodeError) as error:
        parser.error(str(error))
    args.out.parent.mkdir(parents=True, exist_ok=True)
    fit_data = args.out.with_suffix(args.out.suffix + ".reviewed.jsonl")
    fit_data.write_text("".join(json.dumps(row) + "\n" for row in rows))
    cmd = [sys.executable, "-m", "ucci", "fit", "--data", str(fit_data),
           "--tau", str(args.tau), "--c-small", str(args.c_small),
           "--c-large", str(args.c_large), "--cost-model", "sequential",
           "--out", str(args.out)]
    return subprocess.call(cmd)


if __name__ == "__main__":
    raise SystemExit(main())
