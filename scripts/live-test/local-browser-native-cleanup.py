#!/usr/bin/env python3
"""Inspect/stop only birth/executable-bound Chrome children of a native fixture.

No process-name kill, no real browser profile lookup, no credential access.
This helper is part of the independently reviewed fixture, not product recovery.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import signal
import stat
import time


class BsdInfo(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in (
        'flags', 'status', 'xstatus', 'pid', 'ppid', 'uid', 'gid', 'ruid',
        'rgid', 'svuid', 'svgid', 'reserved')]
    _fields_ += [('comm', ctypes.c_char * 16), ('name', ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in (
        'nfiles', 'group', 'jobc', 'device', 'terminal_group')]
    _fields_ += [('nice', ctypes.c_int32), ('birth_sec', ctypes.c_uint64),
                ('birth_usec', ctypes.c_uint64)]


LIB = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
LIB.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                            ctypes.c_void_p, ctypes.c_int]
LIB.proc_pidinfo.restype = ctypes.c_int
LIB.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
LIB.proc_pidpath.restype = ctypes.c_int
LIB.proc_listpids.argtypes = [ctypes.c_uint32, ctypes.c_uint32,
                             ctypes.c_void_p, ctypes.c_int]
LIB.proc_listpids.restype = ctypes.c_int
PREFIXES = ('/Applications/Google Chrome.app/Contents/',
            '/Applications/Microsoft Edge.app/Contents/')


def identity(pid):
    info = BsdInfo()
    size = ctypes.sizeof(info)
    read = LIB.proc_pidinfo(pid, 3, 0, ctypes.byref(info), size)
    if read != size:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return None
        raise RuntimeError('live child identity unreadable')
    if info.status == 5:  # Darwin SZOMB, cannot execute/spawn.
        return None
    if info.uid != os.getuid():
        raise RuntimeError('child uid mismatch')
    path = ctypes.create_string_buffer(4096)
    if LIB.proc_pidpath(pid, path, len(path)) <= 0:
        raise RuntimeError('live child executable unreadable')
    executable = os.fsdecode(path.value)
    if not executable.startswith(PREFIXES):
        raise RuntimeError('process is outside the owned browser bundle')
    return dict(pid=pid, group=int(info.group), birth_sec=int(info.birth_sec),
                birth_usec=int(info.birth_usec), executable=executable)


def members(group):
    pids = (ctypes.c_int * 4096)()
    read = LIB.proc_listpids(2, group, pids, ctypes.sizeof(pids))
    if read < 0 or read >= ctypes.sizeof(pids) or read % ctypes.sizeof(ctypes.c_int):
        raise RuntimeError('owned group inventory unavailable')
    result = []
    for pid in list(pids)[:read // ctypes.sizeof(ctypes.c_int)]:
        current = identity(pid)
        if current:
            if current['group'] != group:
                raise RuntimeError('group changed during inventory')
            result.append(current)
    return result


def load_manifest(path):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_nlink != 1:
        raise RuntimeError('unsafe owned child manifest')
    if metadata.st_size > 2 * 1024 * 1024:
        raise RuntimeError('oversized owned child manifest')
    value = json.loads(path.read_text())
    group = value['group']
    if type(group) is not int or group <= 1:
        raise RuntimeError('invalid owned group')
    known = {}
    for record in value['processes']:
        if (record.get('group') != group or type(record.get('pid')) is not int
                or record['pid'] <= 1 or type(record.get('birth_sec')) is not int
                or type(record.get('birth_usec')) is not int
                or not isinstance(record.get('executable'), str)
                or not record['executable'].startswith(PREFIXES)):
            raise RuntimeError('invalid owned identity')
        if record['pid'] in known:
            raise RuntimeError('duplicate owned pid proof')
        known[record['pid']] = record
    if not known:
        raise RuntimeError('no owned identity proof')
    return group, known


def inspect(group, known):
    current = members(group)
    anchored = any(known.get(record['pid']) == record for record in current)
    for record in current:
        prior = known.get(record['pid'])
        if prior is not None and prior != record:
            raise RuntimeError('owned pid was reused')
        if prior is None and not anchored:
            raise RuntimeError('unproven surviving browser child; no signal sent')
        known[record['pid']] = record
    return current


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--manifest', required=True)
    parser.add_argument('--mode', choices=('check', 'terminate'), default='check')
    arguments = parser.parse_args()
    path = Path(arguments.manifest)
    if (path.name != 'owned-processes.json'
            or not path.parent.name.startswith('ddc-b9b-native-')):
        raise RuntimeError('not an owned native fixture path')
    group, known = load_manifest(path)
    current = inspect(group, known)
    if arguments.mode == 'terminate':
        for decision in (signal.SIGTERM, signal.SIGKILL):
            if not current:
                break
            for record in list(known.values()):
                if identity(record['pid']) == record:
                    try:
                        os.kill(record['pid'], decision)
                    except ProcessLookupError:
                        pass
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                current = inspect(group, known)
                if not current:
                    break
                time.sleep(0.05)
    print(json.dumps({'cleanup_ok': not current, 'remaining': len(current)}))
    return 0 if not current else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as error:
        print(json.dumps({'cleanup_ok': False, 'error': str(error)}))
        raise SystemExit(1)
