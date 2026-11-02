"""Avoid warming release caches for workspace-only version bumps."""

import os
import subprocess

from ci_classify import commit, git, lock_versions_only, text_at, write_outputs


def warm_needed(event: str, head: str) -> bool:
    if event != "push":
        return True
    try:
        head = commit(head)
        parent = git("rev-parse", "--verify", f"{head}^1").strip()
        locks = [(text_at(parent, path), text_at(head, path))
                 for path in ("Cargo.lock", "src-tauri/Cargo.lock")]
        # Manifest/toolchain-only pushes still need warming even with unchanged locks.
        return (all(old == new for old, new in locks)
                or not all(lock_versions_only(old, new) for old, new in locks))
    except (OSError, ValueError, subprocess.SubprocessError):
        return True


if __name__ == "__main__":
    write_outputs({"run": str(warm_needed(os.environ.get("GITHUB_EVENT_NAME", ""),
                                         os.environ.get("HEAD_SHA", ""))).lower()})
