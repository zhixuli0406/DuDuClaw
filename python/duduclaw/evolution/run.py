"""Evolution Engine CLI — called by Rust gateway as subprocess.

Usage:
    python -m duduclaw.evolution.run vet --skill-name NAME [--skills-dir DIR] [--quarantine-dir DIR]

Note: Legacy micro/meso/macro reflection commands have been removed.
Evolution is now driven by the Rust-native prediction engine + GVU loop.
"""

import argparse
import json
import sys
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description="DuDuClaw Evolution Engine")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # Skill vetting
    vet_p = subparsers.add_parser("vet", help="Vet a skill file")
    vet_p.add_argument("--skill-name", required=True)
    vet_p.add_argument("--skills-dir", default="")
    vet_p.add_argument("--quarantine-dir", default="")

    args = parser.parse_args()
    result = run_vet(args)
    print(json.dumps(result, ensure_ascii=False, indent=2))


def run_vet(args: argparse.Namespace) -> dict:
    from .vetter import SkillVetter

    content = sys.stdin.read().strip()
    if not content:
        return {"status": "error", "reason": "no skill content provided"}

    quarantine = Path(args.quarantine_dir) if args.quarantine_dir else None
    vetter = SkillVetter(quarantine_dir=quarantine)

    if args.skills_dir:
        result_status, path = vetter.vet_and_activate(
            args.skill_name, content, Path(args.skills_dir)
        )
        return {
            "status": "ok",
            "result": result_status.value,
            "path": str(path) if path else None,
            "skill_name": args.skill_name,
        }

    result_status, findings = vetter.vet_skill(args.skill_name, content)
    return {
        "status": "ok",
        "result": result_status.value,
        "findings_count": len(findings),
        "findings": [
            {
                "category": f.category,
                "severity": f.severity.value,
                "description": f.description,
                "line": f.line_number,
            }
            for f in findings
        ],
    }


if __name__ == "__main__":
    main()
