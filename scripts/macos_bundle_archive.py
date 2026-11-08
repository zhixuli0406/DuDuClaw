"""Strict, non-extracting gateway app archive validation shared by release gates."""
import re
import stat
from pathlib import PurePosixPath

MAX_BUNDLE_BYTES = 1024 * 1024 * 1024
MAX_EXECUTABLE_BYTES = 768 * 1024 * 1024
MAX_ENTRIES = 4096


def bundle_executable(archive, filename):
    match = re.fullmatch(r'(duduclaw(?:-pro)?)-darwin-(?:arm64|x64)\.app\.zip', filename)
    if not match:
        raise ValueError('unsupported gateway bundle archive name')
    expected = f'DuDuClaw Gateway.app/Contents/MacOS/{match[1]}'
    entries = archive.infolist()
    if len(entries) > MAX_ENTRIES or sum(e.file_size for e in entries) > MAX_BUNDLE_BYTES:
        raise ValueError('bundle exceeds size or entry limit')
    seen = set()
    executable = None
    for entry in entries:
        name = entry.filename
        parts = name.rstrip('/').split('/')
        mode = entry.external_attr >> 16
        kind = stat.S_IFMT(mode)
        if ('\\' in name or any(p in ('', '.', '..') for p in parts)
                or PurePosixPath(name).is_absolute() or name in seen
                or parts[0] != 'DuDuClaw Gateway.app'):
            raise ValueError('unsafe or duplicate bundle member path')
        seen.add(name)
        if kind not in (0, stat.S_IFREG, stat.S_IFDIR) or (entry.is_dir() != (kind == stat.S_IFDIR) and kind != 0):
            raise ValueError('bundle members must be regular files or directories')
        if entry.is_dir():
            continue
        # No other executable or bare gateway can hide among resources.
        if name == expected:
            if entry.file_size >= MAX_EXECUTABLE_BYTES:
                raise ValueError('bundle executable exceeds size limit')
            executable = entry
        elif mode & 0o111 or '/Contents/MacOS/' in name or parts[-1] in ('duduclaw', 'duduclaw-pro', 'duduclaw.exe', 'duduclaw-pro.exe'):
            raise ValueError('bundle must contain exactly one executable at the expected path')
    if executable is None:
        raise ValueError('bundle must contain exactly one executable at the expected path')
    return executable
