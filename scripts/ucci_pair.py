#!/usr/bin/env python3
"""Join UCCI observation rows into a manual-review template.

No correctness label is inferred. Cloud rows, when supplied, must have the
same request_id and a `stage` of `cloud_api`.
"""

import argparse
import json
from pathlib import Path


def read_rows(path: Path) -> dict[tuple[str, str], dict]:
    indexed = {}
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)
        key = (row.get("request_id"), row.get("stage"))
        if not all(isinstance(x, str) and x for x in key):
            raise ValueError(f"{path}:{number}: request_id and stage are required")
        if key in indexed:
            raise ValueError(f"{path}:{number}: duplicate request_id/stage {key}")
        indexed[key] = row
    return indexed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observations", required=True, type=Path)
    parser.add_argument("--cloud-observations", type=Path)
    parser.add_argument("--stage", required=True, choices=("local_fast", "local_strong"))
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    try:
        rows = read_rows(args.observations)
        if args.cloud_observations:
            for key, row in read_rows(args.cloud_observations).items():
                if key in rows:
                    raise ValueError(f"duplicate observation {key} across input files")
                rows[key] = row
    except (OSError, ValueError) as error:
        parser.error(str(error))

    next_stage = "local_strong" if args.stage == "local_fast" else "cloud_api"
    paired = []
    for (request_id, stage), small in rows.items():
        if stage != args.stage:
            continue
        large = rows.get((request_id, next_stage))
        if large is None or small.get("u") is None:
            continue
        if not isinstance(small.get("answer"), str) or not isinstance(large.get("answer"), str):
            continue
        paired.append({
            "id": small["id"], "request_id": request_id, "stage": stage,
            "system_prompt": small.get("system_prompt", ""), "prompt": small.get("prompt", ""),
            "u": small["u"], "answer": small["answer"],
            "large_answer": large["answer"],
            "small_model": small.get("model_id"), "large_model": large.get("model_id"),
            "small_latency_ms": small.get("generation_time_ms"),
            "large_latency_ms": large.get("generation_time_ms"),
            "small_correct": None, "large_correct": None, "label_source": None,
        })
    if not paired:
        parser.error(f"no paired {args.stage} -> {next_stage} rows with a UCCI signal")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("".join(json.dumps(row, ensure_ascii=False) + "\n" for row in paired))
    print(f"wrote {len(paired)} paired rows to {args.out}; review both answers before fitting")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
