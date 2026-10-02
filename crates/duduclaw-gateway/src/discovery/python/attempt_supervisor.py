"""Trusted PID 1: absolute wall deadline, reap and kill all CLI descendants.

Python isolated mode excludes cwd/PYTHONPATH. Exiting namespace PID 1 also
terminates descendants which detached into another session/process group.

Before the CLI starts it also (design DESIGN-discovery-runtime-parity §7):
- creates the per-family state directories under the private HOME
  (codex refuses to start when CODEX_HOME does not exist);
- copies /dudu-runtime/home-seed into the private HOME without following
  symbolic links (directories 0700, files 0600);
- writes DUDU_CREDENTIAL_DOC to HOME/DUDU_CREDENTIAL_DEST (0600) and removes
  both variables from the child environment. A destination that is absolute,
  contains `..` or leaves HOME ends the container with a non-zero code.
"""
import os
import signal
import stat
import subprocess
import sys
import time

HOME = "/tmp/dudu-private/home"
SEED = "/dudu-runtime/home-seed"
STATE_DIRS = (".codex", ".grok")
SETUP_FAILED = 125

child = None
stopped = False


def stop(_signal, _frame):
    global stopped
    stopped = True


def refuse(reason):
    sys.stderr.write("dudu-supervisor: " + reason + "\n")
    raise SystemExit(SETUP_FAILED)


def private_dir(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode):
        refuse("state path is not a directory")
    os.chmod(path, 0o700)


def write_private(path, data):
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = os.open(path, flags, 0o600)
    try:
        os.fchmod(fd, 0o600)
        view = memoryview(data)
        while view:
            view = view[os.write(fd, view):]
    finally:
        os.close(fd)


def copy_seed():
    if not os.path.lexists(SEED):
        return
    if not stat.S_ISDIR(os.lstat(SEED).st_mode):
        refuse("home seed is not a directory")
    for root, dirs, files in os.walk(SEED, followlinks=False):
        relative = os.path.relpath(root, SEED)
        target_root = HOME if relative == "." else os.path.join(HOME, relative)
        private_dir(target_root)
        for name in dirs:
            if stat.S_ISLNK(os.lstat(os.path.join(root, name)).st_mode):
                refuse("home seed contains a link")
        for name in files:
            source = os.path.join(root, name)
            if not stat.S_ISREG(os.lstat(source).st_mode):
                refuse("home seed contains a non-regular file")
            fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
            with os.fdopen(fd, "rb") as handle:
                data = handle.read()
            write_private(os.path.join(target_root, name), data)


def install_credential(environment):
    document = environment.pop("DUDU_CREDENTIAL_DOC", None)
    destination = environment.pop("DUDU_CREDENTIAL_DEST", None)
    if document is None and destination is None:
        return
    if not document or not destination:
        refuse("credential document and destination must come together")
    parts = destination.split("/")
    if os.path.isabs(destination) or any(part in ("", ".", "..") for part in parts):
        refuse("credential destination must stay inside HOME")
    target = os.path.join(HOME, *parts)
    parent = HOME
    for part in parts[:-1]:
        parent = os.path.join(parent, part)
        private_dir(parent)
    resolved = os.path.realpath(os.path.dirname(target))
    if resolved != os.path.dirname(target) or not (resolved + "/").startswith(HOME + "/"):
        refuse("credential destination must stay inside HOME")
    write_private(target, document.encode("utf-8"))


for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(sig, stop)

try:
    deadline = float(sys.argv[1])
    if not deadline > time.time():
        sys.exit(124)
    previous_umask = os.umask(0o077)
    for path in (HOME, "/tmp/dudu-private/tmp"):
        os.makedirs(path, mode=0o700, exist_ok=True)
    for name in STATE_DIRS:
        private_dir(os.path.join(HOME, name))
    copy_seed()
    environment = dict(os.environ)
    install_credential(environment)
    os.environ.pop("DUDU_CREDENTIAL_DOC", None)
    os.environ.pop("DUDU_CREDENTIAL_DEST", None)
    # The CLI keeps the container's original umask for workspace files.
    os.umask(previous_umask)
    child = subprocess.Popen(sys.argv[2:], start_new_session=True, env=environment)
    code = None
    while not stopped and time.time() < deadline:
        code = child.poll()
        if code is not None:
            break
        time.sleep(min(0.02, max(0, deadline - time.time())))
    if code is None:
        code = 124
finally:
    if child is not None:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=1)
        except subprocess.TimeoutExpired:
            pass
sys.exit(code)
