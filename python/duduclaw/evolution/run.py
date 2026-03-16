"""Evolution Engine CLI — called by Rust gateway as subprocess.

Usage:
    python -m duduclaw.evolution.run micro --agent-dir DIR --summary "..."
    python -m duduclaw.evolution.run meso --agent-dir DIR
    python -m duduclaw.evolution.run macro --agent-dir DIR
    python -m duduclaw.evolution.run vet --skill-name NAME --content "..."
"""

import argparse
import asyncio
import json
import sys
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description="DuDuClaw Evolution Engine")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # Micro reflection
    micro_p = subparsers.add_parser("micro", help="Run micro reflection")
    micro_p.add_argument("--agent-id", required=True)
    micro_p.add_argument("--agent-dir", required=True)
    micro_p.add_argument("--summary", default="")

    # Meso reflection
    meso_p = subparsers.add_parser("meso", help="Run meso reflection")
    meso_p.add_argument("--agent-id", required=True)
    meso_p.add_argument("--agent-dir", required=True)

    # Macro reflection
    macro_p = subparsers.add_parser("macro", help="Run macro reflection")
    macro_p.add_argument("--agent-id", required=True)
    macro_p.add_argument("--agent-dir", required=True)

    # Skill vetting
    vet_p = subparsers.add_parser("vet", help="Vet a skill file")
    vet_p.add_argument("--skill-name", required=True)
    vet_p.add_argument("--skills-dir", default="")
    vet_p.add_argument("--quarantine-dir", default="")

    args = parser.parse_args()
    result = asyncio.run(dispatch(args))
    print(json.dumps(result, ensure_ascii=False, indent=2))


async def dispatch(args: argparse.Namespace) -> dict:
    if args.command == "micro":
        return await run_micro(args)
    elif args.command == "meso":
        return await run_meso(args)
    elif args.command == "macro":
        return await run_macro(args)
    elif args.command == "vet":
        return run_vet(args)
    return {"error": f"Unknown command: {args.command}"}


async def run_micro(args: argparse.Namespace) -> dict:
    from .micro import MicroReflection

    agent_dir = Path(args.agent_dir)
    memory_dir = agent_dir / "memory"

    summary = args.summary
    if not summary:
        summary = sys.stdin.read().strip()
    if not summary:
        return {"status": "skipped", "reason": "no summary provided"}

    reflection = MicroReflection(args.agent_id, memory_dir)
    result = await reflection.reflect(summary)
    result["status"] = "ok"
    return result


async def run_meso(args: argparse.Namespace) -> dict:
    from .meso import MesoReflection

    agent_dir = Path(args.agent_dir)
    reflection = MesoReflection(args.agent_id, agent_dir)
    result = await reflection.reflect()
    result["status"] = "ok"
    return result


async def run_macro(args: argparse.Namespace) -> dict:
    from .macro_ import MacroReflection

    agent_dir = Path(args.agent_dir)
    reflection = MacroReflection(args.agent_id, agent_dir)
    result = await reflection.reflect()
    result["status"] = "ok"
    return result


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
