"""Bounded login-state archives, parsed only inside the sandbox container.

The gateway treats the returned uncompressed tar as opaque bytes. This helper
runs as the unprivileged browser user, while Chromium is not running. It never
restores browser preferences, extensions, passwords, service workers or caches.
"""

import io
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import tarfile
import tempfile

MAX_FILES = 4096
HARD_MAX_BYTES = 128 * 1024 * 1024
FILES = {"Default/Cookies", "Default/Cookies-journal"}
TREES = ("Default/Local Storage/leveldb", "Default/IndexedDB")


def permitted(name, directory=False):
    parts = name.split("/")
    if any(p in ("", ".", "..") for p in parts) or "\\" in name or "\x00" in name:
        return False
    if len(name.encode()) > 1024 or PurePosixPath(name).is_absolute():
        return False
    if directory:
        return any(tree == name or tree.startswith(name + "/") or name.startswith(tree + "/")
                   for tree in TREES) or name == "Default"
    return name in FILES or any(name.startswith(tree + "/") for tree in TREES)


def check_limit(limit):
    if not 1 <= limit <= HARD_MAX_BYTES:
        raise ValueError("invalid profile byte limit")


def import_archive(blob, destination, limit):
    """Validate the whole bounded tar before replacing an unused profile."""
    check_limit(limit)
    if len(blob) > limit:
        raise ValueError("profile archive too large")
    destination = Path(destination)
    try:
        tar = tarfile.open(fileobj=io.BytesIO(blob), mode="r:")
        with tar:
            members, names, total = [], set(), 0
            for member in tar:
                if len(members) >= MAX_FILES or member.name in names:
                    raise ValueError("too many or duplicate profile entries")
                if not (member.isfile() or member.isdir()):
                    raise ValueError("profile links and special files are refused")
                if not permitted(member.name, member.isdir()) or member.size < 0:
                    raise ValueError("profile entry outside login-state allowlist")
                if member.isdir() and member.size:
                    raise ValueError("nonempty directory entry")
                total += member.size
                if total > limit:
                    raise ValueError("profile data too large")
                names.add(member.name)
                members.append(member)
            # Fresh staging, no archive-supplied links or metadata. No tar
            # extract/extractall: only regular bytes with fixed permissions.
            destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            with tempfile.TemporaryDirectory(prefix=".profile-", dir=destination.parent) as tmp:
                stage = Path(tmp) / "data"
                stage.mkdir(mode=0o700)
                for member in members:
                    target = stage / member.name
                    if member.isdir():
                        target.mkdir(parents=True, exist_ok=True, mode=0o700)
                        continue
                    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    with tar.extractfile(member) as source:
                        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                        with os.fdopen(fd, "wb") as output:
                            remaining = member.size
                            while remaining:
                                chunk = source.read(min(65536, remaining))
                                if not chunk:
                                    raise ValueError("truncated profile entry")
                                output.write(chunk)
                                remaining -= len(chunk)
                # Chromium is stopped. Symlinks at the destination are
                # unlinked, never followed, and all work runs as sandbox.
                if destination.is_symlink():
                    destination.unlink()
                elif destination.exists():
                    shutil.rmtree(destination)
                stage.rename(destination)
    except (tarfile.TarError, EOFError) as error:
        raise ValueError("invalid profile archive") from error


def export_archive(source, limit):
    """Only read allowlisted regular files; links and special files fail."""
    check_limit(limit)
    source = Path(source)
    entries, total = [], 0
    if source.is_symlink() or (source.exists() and not source.is_dir()):
        raise ValueError("invalid profile root")
    if (source / "Default").is_symlink():
        raise ValueError("profile directory link refused")

    def visit(path, relative):
        nonlocal total
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            if not permitted(relative, directory=True):
                raise ValueError("invalid profile directory")
            for child in sorted(path.iterdir()):
                visit(child, relative + "/" + child.name)
            return
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or not permitted(relative):
            raise ValueError("profile links and special files are refused")
        total += info.st_size
        if total > limit or len(entries) >= MAX_FILES:
            raise ValueError("profile data too large")
        entries.append((path, relative, info.st_size))

    for relative in sorted(FILES | set(TREES)):
        path = source / relative
        # lstat catches dangling symlinks too. Verify every intermediate
        # directory, including Local Storage, before traversing it.
        for parent in path.parents:
            if parent == source:
                break
            if parent.is_symlink():
                raise ValueError("profile directory link refused")
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        visit(path, relative)
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for path, relative, size in entries:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, "rb") as data:
                current = os.fstat(data.fileno())
                if not stat.S_ISREG(current.st_mode) or current.st_nlink != 1 or current.st_size != size:
                    raise ValueError("profile changed while exporting")
                member = tarfile.TarInfo(relative)
                member.size, member.mode = size, 0o600
                tar.addfile(member, data)
            if out.tell() > limit:
                raise ValueError("profile archive too large")
    if out.tell() > limit:
        raise ValueError("profile archive too large")
    return out.getvalue()


def main():
    if os.geteuid() == 0:
        raise ValueError("profile archive helper must run as sandbox")
    action, raw_limit = sys.argv[1:]
    limit = int(raw_limit)
    check_limit(limit)
    path = Path("/tmp/sandbox-home/chromium-profile")
    if action == "import":
        data = sys.stdin.buffer.read(limit + 1)
        # Empty stdin is the gateway's explicit fresh-profile handshake.
        # The container is new and Chromium has not opened its profile yet.
        if data:
            import_archive(data, path, limit)
    elif action == "export":
        sys.stdout.buffer.write(export_archive(path, limit))
    else:
        raise ValueError("invalid profile action")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        # Never echo archive bytes, paths supplied by the archive or cookies.
        print("profile operation refused: " + type(error).__name__, file=sys.stderr)
        sys.exit(1)
