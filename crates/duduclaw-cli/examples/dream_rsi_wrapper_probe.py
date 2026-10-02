#!/usr/bin/env python3
"""No-API regression probes for the V2 driver's exact embedded wrapper."""
import ast
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time

source = Path(__file__).with_name("dream_rsi_live_validation.rs").read_text()
script = re.search(r'const CLAUDE_WRAPPER: &str = r#"(.*?)"#;', source, re.S).group(1)
ast.parse(script)

FAKE = r'''#!/usr/bin/env python3
import json,os,sys,time,subprocess
from pathlib import Path
home=Path(os.environ['DUDUCLAW_HOME']); p=home/'mock-spawns'
p.write_text(str(int(p.read_text())+1) if p.exists() else '1')
assert sys.argv[sys.argv.index('--max-turns')+1]=='4'
assert sys.argv[sys.argv.index('--tools')+1]==('Write' if os.environ['DUDU_V2_LIVE_KIND']=='fork' else '')
for flag in ['--disable-slash-commands','--no-session-persistence']: assert flag in sys.argv
for flag in ['--dangerously-skip-permissions','--allow-dangerously-skip-permissions']: assert flag not in sys.argv
if os.environ['DUDU_V2_LIVE_KIND']=='fork':
    assert '--safe-mode' in sys.argv and '--restricted' not in sys.argv
else:
    assert '--restricted' in sys.argv and '--safe-mode' not in sys.argv
    assert Path.cwd()==(home/'agents'/'v2-worker').resolve()
    assert 'CLAUDE_CODE_SAFE_MODE' not in os.environ
assert 'CLAUDE_CODE_SIMPLE' not in os.environ
mode=os.environ.get('MOCK_CASE')
if mode in ['pid_cancel','normal_redirected','normal_inherited']:
    output={'stdout':subprocess.DEVNULL,'stderr':subprocess.DEVNULL} if mode=='normal_redirected' else {}
    child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(20)'],**output)
    (home/'mock-descendant-pid').write_text(str(child.pid))
(home/'mock-pid').write_text(str(os.getpid()))
(home/'mock-group').write_text(str(os.getpgrp()))
if mode=='stderr':
    sys.stderr.write('Error: usage limit reached\n'+'x'*131072); sys.stderr.flush(); time.sleep(20)
elif mode=='overflow':
    sys.stdout.write('x'*1048577); sys.stdout.flush(); time.sleep(20)
elif mode in ['sleep','pid_cancel']: time.sleep(20)
else:
    print(json.dumps({'type':'rate_limit_event','rate_limit_info':{'status':'allowed_warning'}}),flush=True)
    if mode=='assistant': print(json.dumps({'type':'assistant','error':'rate_limit'}),flush=True)
    elif mode=='errors': print(json.dumps({'type':'result','errors':['429 too many requests']}),flush=True)
    elif mode=='error': print(json.dumps({'type':'result','error':'usage limit reached'}),flush=True)
    else: print(json.dumps({'type':'result','is_error':mode=='terminal','result':'rate limit reached' if mode=='terminal' else 'synthetic success'}),flush=True)
'''


def evidence(case):
    print(json.dumps({"mock_wrapper_case": case, "passed": True, "real_llm_calls": 0}))


def cannot_execute(pid):
    # A reparented zombie can briefly remain in ps; it cannot run or use quota.
    result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True)
    return not result.stdout.strip() or result.stdout.strip().startswith("Z")


with tempfile.TemporaryDirectory(prefix="dudu-v2-wrapper-mock-", dir="/tmp") as base:
    base = Path(base)
    wrapper = base / "claude"
    wrapper.write_text(script)
    wrapper.chmod(0o700)
    fake = base / "fake-claude"
    fake.write_text(FAKE)
    fake.chmod(0o700)

    def fixture(case):
        home = base / case
        home.mkdir()
        (home / 'agents' / 'v2-worker').mkdir(parents=True)
        mcp = home / "mcp.json"
        mcp.write_text('{"mcpServers":{}}')
        bootstrap = wrapper.with_name("validation-bootstrap.json")
        bootstrap.write_text(json.dumps({"home":str(home),"real":str(fake),
                                        "kind":"fork" if case == "fork" else "goal",
                                        "mcp_config":str(mcp)}))
        bootstrap.chmod(0o600)
        env = dict(os.environ, DUDUCLAW_HOME=str(home), DUDU_V2_REAL_CLAUDE=str(fake),
                   DUDU_V2_MCP_CONFIG=str(mcp), DUDU_V2_LIVE_KIND="fork" if case == "fork" else "goal",
                   MOCK_CASE=case, CLAUDE_CODE_SAFE_MODE="1", CLAUDE_CODE_SIMPLE="1")
        args = [str(wrapper), "-p", "synthetic", "--dangerously-skip-permissions",
                "--allow-dangerously-skip-permissions"]
        if case != "fork":
            args += ["--model", "claude-haiku-4-5"]
        return home, env, args

    for case in ["normal", "terminal", "assistant", "errors", "error", "stderr", "overflow", "fork"]:
        home, env, args = fixture(case)
        started = time.monotonic()
        result = subprocess.run(args, env=env, capture_output=True, timeout=8)
        diagnostic = json.loads((home / "live-call-1.json").read_text())
        assert "synthetic" not in json.dumps(diagnostic)
        assert diagnostic["exit_code"] is not None
        for name in ["stdout", "stderr"]:
            logfile = home / f"live-call-1.{name}"
            assert logfile.stat().st_size <= 262144
            assert logfile.stat().st_mode & 0o777 == 0o600
        if case == "overflow":
            assert diagnostic["stdout_truncated"]
        if case == "stderr":
            assert b"usage limit reached" in (home / "live-call-1.stderr").read_bytes()
        if case in ["normal", "fork"]:
            assert result.returncode == 0 and not (home / "live-stop").exists()
            assert subprocess.run(args, env=env, capture_output=True, timeout=8).returncode == 0
            assert subprocess.run(args, env=env, capture_output=True, timeout=8).returncode != 0
            assert (home / "mock-spawns").read_text() == "2"
            assert (home / "live-stop").read_text().startswith("[hard_bound]")
        else:
            assert result.returncode != 0 and (home / "live-stop").exists()
            assert subprocess.run(args, env=env, capture_output=True, timeout=8).returncode != 0
            assert (home / "mock-spawns").read_text() == "1"
            assert time.monotonic() - started < 4
            expected = "[hard_bound]" if case == "overflow" else "[rate_limit]"
            assert (home / "live-stop").read_text().startswith(expected)
        evidence(case)

    for case in ["sleep", "pid_cancel"]:
        home, env, args = fixture(case)
        # No pre-created process group: mirrors production fork kill_on_drop.
        process = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while not (home / "mock-pid").exists() and time.monotonic() < deadline:
                time.sleep(.01)
            child = int((home / "mock-pid").read_text())
            managed_group = int((home / "mock-group").read_text())
            assert os.getpgid(child) == managed_group and managed_group != process.pid
            descendants = [child]
            if case == "pid_cancel":
                descendants.append(int((home / "mock-descendant-pid").read_text()))
                process.kill()  # Exact production direct-child SIGKILL cancellation.
            else:
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
            deadline = time.monotonic() + 5
            while not all(cannot_execute(pid) for pid in descendants) and time.monotonic() < deadline:
                time.sleep(.02)
            assert all(cannot_execute(pid) for pid in descendants)
            evidence("direct_pid_cancel_guardian" if case == "pid_cancel" else "same_group_sigkill")
        finally:
            # Do not leave a mock sleeper behind even when a regression fails.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                os.killpg(int((home / "mock-group").read_text()), signal.SIGKILL)
            except (ProcessLookupError, FileNotFoundError):
                pass
            process.wait(timeout=5)

    for case in ["normal_redirected", "normal_inherited"]:
        home, env, args = fixture(case)
        started = time.monotonic()
        result = subprocess.run(args, env=env, capture_output=True, timeout=8)
        try:
            assert result.returncode == 0 and not (home / "live-stop").exists()
            assert time.monotonic() - started < 4
            descendant = int((home / "mock-descendant-pid").read_text())
            assert cannot_execute(descendant)
            evidence(case)
        finally:
            try:
                os.killpg(int((home / "mock-group").read_text()), signal.SIGKILL)
            except (ProcessLookupError, FileNotFoundError):
                pass

    home, env, args = fixture("stopped")
    (home / "live-stop").write_text("pre-existing terminal limit")
    assert subprocess.run(args, env=env, capture_output=True, timeout=8).returncode != 0
    assert not (home / "mock-spawns").exists()
    evidence("preexisting_stop_no_spawn")

    home, env, args = fixture("model_spoof")
    args[args.index("--model")+1] = "opus-haiku-spoof"
    assert subprocess.run(args, env=env, capture_output=True, timeout=8).returncode != 0
    assert not (home / "mock-spawns").exists()
    assert not (home / "live-call-count").exists()
    assert (home / "live-stop").read_text().startswith("[hard_bound]")
    evidence("model_spoof_no_spawn_or_charge")

    home, env, args = fixture("scrubbed_env")
    # Reproduce production env_clear: none of the DUDUCLAW_HOME / DUDU_V2_*
    # bootstrap variables survives. The fake CLI must still launch twice.
    stripped = {key:env[key] for key in ["PATH", "HOME", "USER", "LOGNAME"] if key in env}
    assert subprocess.run(args, env=stripped, capture_output=True, timeout=8).returncode == 0
    assert subprocess.run(args, env=stripped, capture_output=True, timeout=8).returncode == 0
    assert subprocess.run(args, env=stripped, capture_output=True, timeout=8).returncode != 0
    assert (home / "mock-spawns").read_text() == "2"
    evidence("scrubbed_env_private_bootstrap")

    home, env, args = fixture("utility_flags")
    args += ["--dangerously-skip-permissions", "--allow-dangerously-skip-permissions",
             "--permission-mode", "bypassPermissions"]
    assert subprocess.run(args, env=env, capture_output=True, timeout=8).returncode == 0
    diagnostic = json.loads((home / "live-call-1.json").read_text())
    assert "--dangerously-skip-permissions" in diagnostic["original_flags"]
    assert not (home / "live-stop").exists()
    evidence("restricted_mode_strips_legacy_utility_permission_bypass")

    home, env, args = fixture("driver_failed")
    env["MOCK_CASE"] = "sleep"
    process = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 5
        while not (home / "mock-pid").exists() and time.monotonic() < deadline:
            time.sleep(.01)
        child = int((home / "mock-pid").read_text())
        marker = "[driver_failed] synthetic dispatcher failure"
        (home / "live-stop").write_text(marker)
        process.kill()
        process.wait(timeout=5)
        deadline = time.monotonic() + 5
        while not cannot_execute(child) and time.monotonic() < deadline:
            time.sleep(.02)
        assert cannot_execute(child)
        assert (home / "live-stop").read_text() == marker
        evidence("driver_failure_guard_not_reclassified_as_quota")
    finally:
        try:
            os.killpg(int((home / "mock-group").read_text()), signal.SIGKILL)
        except (ProcessLookupError, FileNotFoundError):
            pass
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
print(json.dumps({"wrapper_ast": "passed", "real_llm_calls": 0}))
