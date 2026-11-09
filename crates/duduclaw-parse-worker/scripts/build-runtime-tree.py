#!/usr/bin/env python3
"""Build a new parse-worker runtime tree after the installer verifies its signature.

The required SHA-256 comes from the verified release manifest, never from a
worker-provided value. This helper does not verify release signatures or activate
units. It refuses an existing destination and copies no host directories.
"""
import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import tempfile

WORKER_TARGET = PurePosixPath('/usr/libexec/duduclaw/duduclaw-parse-worker')
LOADERS = {'/lib64/ld-linux-x86-64.so.2', '/lib/ld-linux-aarch64.so.1'}
MAX_ELF_BYTES = 256 * 1024 * 1024


def trusted_file(path):
    resolved = Path(path).resolve(strict=True)
    for ancestor in (resolved, *resolved.parents):
        info = ancestor.lstat()
        if info.st_uid != 0 or info.st_gid != 0 or info.st_mode & 0o022:
            raise ValueError('runtime input or ancestor is not root-owned and protected')
    info = resolved.stat()
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_ELF_BYTES:
        raise ValueError('runtime input must be a bounded regular file')
    return resolved


def dependency_paths(output):
    paths = set()
    for line in output.splitlines():
        line = line.strip()
        if not line:
            continue
        if re.fullmatch(r'linux-vdso\.so\.1 \(0x[0-9a-fA-F]+\)', line):
            continue
        match = re.fullmatch(r'(?:[^ /]+ => )?(/[^ ]+) \(0x[0-9a-fA-F]+\)', line)
        if match is None:
            raise ValueError('unresolved or unexpected loader dependency')
        path = PurePosixPath(match[1])
        if (len(match[1]) > 512 or len(path.parts) > 8
                or '..' in path.parts or str(path) != match[1]
                or not str(path).startswith(('/lib/', '/lib64/', '/usr/lib/', '/usr/lib64/'))
                or str(path).startswith('/usr/lib/duduclaw/')):
            raise ValueError('dependency is outside system runtime library directories')
        paths.add(str(path))
    if len(paths) > 64:
        raise ValueError('runtime dependency closure exceeds limit')
    return paths


def inspect_closure(worker):
    environment = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'}
    trusted_file('/usr/bin/readelf')
    trusted_file('/usr/bin/readelf')
    headers = subprocess.run(['/usr/bin/readelf', '-lW', str(worker)], check=True,
                             capture_output=True, text=True, env=environment, timeout=30).stdout
    interpreters = re.findall(r'\[Requesting program interpreter: ([^\]]+)\]', headers)
    if not interpreters:
        # A static ELF has no interpreter or needed shared objects.
        dynamic = subprocess.run(['/usr/bin/readelf', '-dW', str(worker)], check=True,
                                 capture_output=True, text=True, env=environment, timeout=30).stdout
        if '(NEEDED)' in dynamic:
            raise ValueError('dynamic ELF has no interpreter')
        return set()
    if len(interpreters) != 1 or interpreters[0] not in LOADERS:
        raise ValueError('unsupported runtime interpreter')
    loader = interpreters[0]
    trusted_file(loader)
    # --list does not run the worker. Strip LD_PRELOAD/LD_LIBRARY_PATH and all
    # other caller environment; only the already-verified worker is inspected.
    listing = subprocess.run([loader, '--list', str(worker)], check=True,
                             capture_output=True, text=True, env=environment, timeout=30).stdout
    paths = dependency_paths(listing)
    paths.add(loader)
    return paths


def protected_destination(destination):
    parent = destination.parent.resolve(strict=True)
    for ancestor in (parent, *parent.parents):
        info = ancestor.stat()
        if info.st_uid != 0 or info.st_gid != 0 or info.st_mode & 0o022:
            raise ValueError('destination parent is not protected')
    if destination.parent != parent:
        raise ValueError('destination parent must be canonical')


def runtime_inputs(worker, expected_sha256):
    if re.fullmatch(r'[0-9a-f]{64}', expected_sha256) is None:
        raise ValueError('expected worker digest must come from verified manifest')
    worker = trusted_file(worker)
    if hashlib.sha256(worker.read_bytes()).hexdigest() != expected_sha256:
        raise ValueError('worker differs from verified manifest')
    if worker.read_bytes()[:4] != b'\x7fELF':
        raise ValueError('worker is not ELF')
    inputs = {str(WORKER_TARGET): worker}
    for target in inspect_closure(worker):
        inputs[target] = trusted_file(target)
    return inputs


def build_tree(worker, expected_sha256, destination):
    inputs = runtime_inputs(worker, expected_sha256)
    destination = Path(destination)
    # The installer chooses a fresh root-owned version directory, never an
    # employee-controlled parent or an existing tree containing unknown files.
    protected_destination(destination)
    destination.mkdir(mode=0o755)
    destination.chmod(0o755)
    for target, source in sorted(inputs.items()):
        output = destination.joinpath(*PurePosixPath(target).parts[1:])
        output.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
        for directory in output.parent.parents:
            if directory == destination:
                break
            directory.chmod(0o755)
        output.parent.chmod(0o755)
        with source.open('rb') as src, output.open('xb') as dst:
            shutil.copyfileobj(src, dst)
        output.chmod(0o755)
        with output.open('rb') as copied:
            os.fsync(copied.fileno())
    # No symlinks or broad binds are needed, including merged-/usr systems.
    # Recheck the copied worker before publication; errors leave an inactive tree.
    if hashlib.sha256(destination.joinpath(*WORKER_TARGET.parts[1:]).read_bytes()).hexdigest() != expected_sha256:
        raise ValueError('worker changed while constructing runtime tree')
    for directory, _, _ in os.walk(destination, topdown=False):
        sync_directory(Path(directory))
    return sorted(inputs)


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def protected_tree_entry(info):
    return (info.st_uid == 0 and info.st_gid == 0
            and stat.S_IMODE(info.st_mode) == 0o755)


def verify_tree(worker, expected_sha256, destination):
    """Read back an existing tree without trusting a self-written inventory."""
    inputs = runtime_inputs(worker, expected_sha256)
    destination = Path(destination)
    protected_destination(destination)
    expected_files = {target.lstrip('/'): source for target, source in inputs.items()}
    expected_directories = {'.'}
    for target in expected_files:
        expected_directories.update(str(p) for p in PurePosixPath(target).parents)
    pending = [destination]
    root_device = destination.lstat().st_dev
    seen = 1
    observed = set()
    while pending:
        path = pending.pop()
        info = path.lstat()
        relative = str(path.relative_to(destination))
        if not protected_tree_entry(info) or info.st_dev != root_device:
            raise ValueError('runtime tree ownership or mode differs')
        if stat.S_ISDIR(info.st_mode):
            if relative not in expected_directories:
                raise ValueError('unexpected runtime directory')
            for child in path.iterdir():
                seen += 1
                if seen > 1024:
                    raise ValueError('runtime tree inventory exceeds limit')
                pending.append(child)
        elif stat.S_ISREG(info.st_mode):
            source = expected_files.get(relative)
            if source is None or info.st_nlink != 1 or info.st_size > MAX_ELF_BYTES:
                raise ValueError('unexpected runtime file')
            expected = (bytes.fromhex(expected_sha256) if relative == str(WORKER_TARGET).lstrip('/')
                        else hashlib.sha256(source.read_bytes()).digest())
            if hashlib.sha256(path.read_bytes()).digest() != expected:
                raise ValueError('runtime tree content differs')
            observed.add(relative)
        else:
            raise ValueError('runtime tree contains a link or special entry')
    if observed != set(expected_files):
        raise ValueError('runtime tree is incomplete')
    return sorted(inputs)


def install_tree(worker, expected_sha256, destination):
    """Publish a fresh tree atomically; retain and verify any populated tree.

    A mismatching existing tree requires the signed generation coordinator to
    replace it while admission is closed. This helper never deletes old content.
    """
    destination = Path(destination)
    protected_destination(destination)
    if destination.exists() or destination.is_symlink():
        info = destination.lstat()
        if not stat.S_ISDIR(info.st_mode) or not protected_tree_entry(info):
            raise ValueError('unsafe existing runtime tree')
        if next(destination.iterdir(), None) is not None:
            return verify_tree(worker, expected_sha256, destination)
    # The random staging parent is created by this invocation and never shared.
    # Failure preserves its inactive tree for operator inspection.
    stage = Path(tempfile.mkdtemp(prefix='.parse-runtime-', dir=destination.parent))
    stage.chmod(0o700)
    tree = stage / 'tree'
    build_tree(worker, expected_sha256, tree)
    verify_tree(worker, expected_sha256, tree)
    os.replace(tree, destination)
    sync_directory(destination.parent)
    stage.rmdir()
    sync_directory(destination.parent)
    return verify_tree(worker, expected_sha256, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, default=Path(str(WORKER_TARGET)))
    parser.add_argument('--worker-sha256', required=True)
    parser.add_argument('--install', action='store_true')
    parser.add_argument('--verify', action='store_true')
    parser.add_argument('--destination', type=Path, default=Path('/usr/lib/duduclaw/parse-root-tree'))
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error('runtime tree construction requires the root installer')
    if args.install and args.verify:
        parser.error('choose install or verify')
    operation = verify_tree if args.verify else install_tree if args.install else build_tree
    operation(args.worker, args.worker_sha256, args.destination)


if __name__ == '__main__':
    main()
