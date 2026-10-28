#!/usr/bin/env python3
"""Read main platform CI before a release; never mutate Git or GitHub state."""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import subprocess

REPO = "zhixuli0406/DuDuClaw"
MAX_COMMITS_BEHIND = 3
MAX_AGE = dt.timedelta(hours=36)
PLATFORM_JOBS = ("Rust macos-latest", "Rust windows-latest")


def command(*args):
    env = None
    if args[0] == "git":
        args = ("git", "--no-replace-objects", *args[1:])
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    return subprocess.check_output(args, text=True, stderr=subprocess.DEVNULL,
                                   timeout=60, env=env)


def ancestor(root, older, newer):
    try:
        command("git", "-C", str(root), "merge-base", "--is-ancestor", older, newer)
        return True
    except subprocess.CalledProcessError as error:
        if error.returncode == 1:
            return False
        raise


def check(root, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    remote = command("git", "-C", str(root), "ls-remote", "origin", "refs/heads/main").split()
    if len(remote) != 2 or remote[1] != "refs/heads/main" or not re.fullmatch(r"[0-9a-f]{40}", remote[0]):
        raise ValueError("cannot determine remote main tip")
    tip = remote[0]
    if not ancestor(root, tip, "HEAD"):
        raise ValueError(f"release HEAD is missing remote main tip {tip}; update release source")
    runs = json.loads(command(
        "gh", "run", "list", "--repo", REPO, "--workflow", "ci.yml", "--branch", "main",
        "--limit", "100", "--json",
        "databaseId,headSha,event,createdAt,status,conclusion"))
    eligible = []
    for run in runs:
        if run["event"] not in {"push", "schedule"}:
            continue
        sha = run["headSha"]
        if not re.fullmatch(r"[0-9a-f]{40}", sha):
            raise ValueError("invalid CI source SHA")
        created = dt.datetime.fromisoformat(run["createdAt"].replace("Z", "+00:00"))
        if created.tzinfo is None:
            raise ValueError("CI creation time lacks timezone")
        if ancestor(root, sha, "HEAD"):
            eligible.append((created, run))
    eligible.sort(key=lambda entry: (entry[0], entry[1]["databaseId"]), reverse=True)
    for created, run in eligible:
        sha = run["headSha"]
        if run["status"] != "completed":
            raise ValueError(f"CI still running on {sha}; wait (status={run['status']})")
        if run["conclusion"] in {"cancelled", "skipped"}:
            # Only a later run on a strictly newer main commit proves supersession.
            if any(newer_created > created and newer["headSha"] != sha and
                   ancestor(root, sha, newer["headSha"])
                   for newer_created, newer in eligible):
                continue
            raise ValueError(f"main CI run {run['databaseId']} ({sha}): {run['conclusion']}; no newer main commit run supersedes it")
        if run["conclusion"] != "success":
            raise ValueError(f"main CI run {run['databaseId']} ({sha}): conclusion={run['conclusion']}")
        if not ancestor(root, sha, tip):
            raise ValueError(f"main CI source {sha} is not an ancestor of remote main tip {tip}")
        behind = int(command("git", "-C", str(root), "rev-list", "--count", sha + ".." + tip))
        if behind > MAX_COMMITS_BEHIND:
            raise ValueError(f"newest eligible main CI is {behind} commits behind remote main (maximum {MAX_COMMITS_BEHIND})")
        if not dt.timedelta(0) <= now - created <= MAX_AGE:
            raise ValueError("newest eligible main CI is stale or future-dated (maximum age 36 hours)")
        jobs = json.loads(command("gh", "run", "view", str(run["databaseId"]),
                                  "--repo", REPO, "--json", "jobs"))["jobs"]
        bad = []
        for name in PLATFORM_JOBS:
            matching = [job for job in jobs if job["name"] == name]
            if len(matching) != 1 or matching[0].get("conclusion") != "success":
                bad.append(name + "=" + str(matching[0].get("conclusion", "missing") if matching else "missing"))
        if bad:
            raise ValueError(f"main CI run {run['databaseId']} ({sha[:12]}, {behind} commits behind): " + ", ".join(bad))
        return f"main CI run {run['databaseId']} ({sha[:12]}, {behind} commits behind remote main): macOS + Windows success"
    raise ValueError("no push/schedule main CI ancestor found within the latest 100 runs")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--lane", choices=("minor", "major", "hotfix"), required=True)
    args = parser.parse_args()
    override = os.environ.get("DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI", "").strip()
    try:
        print("Platform CI preflight: PASS — " + check(args.root))
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        detail = str(error) if isinstance(error, ValueError) else "gh/Git query unavailable, unauthenticated or malformed"
        if args.lane == "hotfix":
            print("WARNING: Platform CI preflight: " + detail + "; hotfix continues.")
        elif override:
            print("WARNING: Platform CI override: " + override + " — " + detail)
        else:
            parser.exit(1, "Platform CI preflight: BLOCKED — " + detail +
                        ". Resolve CI or set DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI=\"<reason>\".\n")


if __name__ == "__main__":
    main()
