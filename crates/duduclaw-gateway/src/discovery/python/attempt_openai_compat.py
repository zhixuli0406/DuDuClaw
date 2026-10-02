"""Trusted minimal OpenAI-compatible tool loop, executed inside the container.

Only files and shell exist as tools. No MCP, Web or subagent provider tools.
The PID-1 supervisor owns hard deadline/namespace cleanup. This is not a dollar
ceiling: a sent generation cannot be recalled, so strict USD is refused outside.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import urllib.error
import urllib.request

MAX_RESPONSE = 4 * 1024 * 1024
MAX_TOOL_OUTPUT = 64 * 1024


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def execute_tool(name, arguments):
    if not isinstance(arguments, dict):
        raise ValueError("tool arguments must be an object")
    if name == "read_file":
        with open(arguments["path"], "rb") as source:
            return source.read(MAX_TOOL_OUTPUT).decode("utf-8", "replace")
    if name == "write_file":
        content = arguments["content"]
        if not isinstance(content, str) or len(content.encode()) > MAX_TOOL_OUTPUT:
            raise ValueError("write content exceeds limit")
        Path(arguments["path"]).write_text(content)
        return "written"
    if name == "list_directory":
        return json.dumps(sorted(os.listdir(arguments["path"]))[:1000])[:MAX_TOOL_OUTPUT]
    if name == "shell":
        command = arguments["command"]
        if not isinstance(command, str) or len(command) > MAX_TOOL_OUTPUT:
            raise ValueError("invalid shell command")
        # Disk use is bounded by the container's tmpfs cap, avoiding an
        # unbounded communicate() buffer while preserving both error streams.
        import tempfile
        with tempfile.TemporaryFile() as output:
            child = subprocess.Popen(["/bin/sh", "-c", command], stdout=output, stderr=output, start_new_session=True)
            try:
                child.wait(timeout=30)
            except subprocess.TimeoutExpired:
                pass
            finally:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait()
            output.seek(0)
            return output.read(MAX_TOOL_OUTPUT).decode("utf-8", "replace")
    raise ValueError("unsupported tool")


def run(model, max_turns):
    tools = []
    specs = [
        ("read_file", {"path": {"type": "string"}}, ["path"]),
        ("write_file", {"path": {"type": "string"}, "content": {"type": "string"}}, ["path", "content"]),
        ("list_directory", {"path": {"type": "string"}}, ["path"]),
        ("shell", {"command": {"type": "string"}}, ["command"]),
    ]
    for name, properties, required in specs:
        tools.append({"type": "function", "function": {"name": name,
            "description": "Operate on the isolated attempt workspace.",
            "parameters": {"type": "object", "properties": properties,
                           "required": required, "additionalProperties": False}}})
    messages = [{"role": "user", "content": sys.stdin.read(1024 * 1024)}]
    usage = {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0}
    usage_known = True
    endpoint = os.environ["DUDU_ATTEMPT_BASE_URL"].rstrip("/") + "/chat/completions"
    key = os.environ["OPENAI_API_KEY"]
    # Explicit proxy-free opener: ambient proxy variables are never consulted.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            raise urllib.error.HTTPError(req.full_url, code, "provider redirect refused", headers, fp)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    for turn in range(max_turns):
        payload = {"model": model, "messages": messages, "tools": tools,
                   "tool_choice": "auto", "max_tokens": 4096, "stream": False}
        request = urllib.request.Request(endpoint, data=json.dumps(payload).encode(),
            headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"})
        try:
            with opener.open(request, timeout=120) as response:
                raw = response.read(MAX_RESPONSE + 1)
        except urllib.error.HTTPError as error:
            # Error status is authoritative. Never retry a 429 inside this loop.
            emit({"type": "result", "is_error": True,
                  "error": {"code": error.code, "message": "provider HTTP error"}})
            return 1
        if len(raw) > MAX_RESPONSE:
            raise ValueError("provider response exceeds limit")
        body = json.loads(raw)
        current = body.get("usage")
        turn_usage = None
        if isinstance(current, dict) and all(isinstance(current.get(k), int) and current[k] >= 0
                                            for k in ["prompt_tokens", "completion_tokens"]):
            cached = current.get("prompt_tokens_details", {}).get("cached_tokens", 0)
            cached = cached if isinstance(cached, int) and 0 <= cached <= current["prompt_tokens"] else 0
            turn_usage = {"input_tokens": current["prompt_tokens"] - cached, "output_tokens": current["completion_tokens"], "cache_read_input_tokens": cached}
            usage["input_tokens"] += current["prompt_tokens"] - cached
            usage["cache_read_input_tokens"] += cached
            usage["output_tokens"] += current["completion_tokens"]
        else:
            usage_known = False
        message = body["choices"][0]["message"]
        messages.append(message)
        observed = body.get("model")
        calls = message.get("tool_calls", [])
        emit({"type": "assistant", "message": {"id": "compat-turn-%d" % turn, "usage": turn_usage, "model": observed,
              "content": [{"type": "tool_use", "name": call["function"]["name"]} for call in calls]}})
        if not calls:
            emit({"type": "result", "is_error": False,
                  "result": message.get("content") or "",
                  "usage": usage if usage_known else None})
            return 0
        for call in calls:
            name = call["function"]["name"]
            try:
                output = execute_tool(name, json.loads(call["function"]["arguments"]))
            except (OSError, ValueError, KeyError) as error:
                output = "tool error: " + type(error).__name__
            messages.append({"role": "tool", "tool_call_id": call["id"], "content": output})
    emit({"type": "result", "is_error": True, "result": "tool loop turn limit reached",
          "usage": usage if usage_known else None})
    return 1


if __name__ == "__main__":
    try:
        sys.exit(run(sys.argv[1], int(sys.argv[2])))
    except (OSError, ValueError, KeyError, IndexError) as error:
        emit({"type": "result", "is_error": True, "error": type(error).__name__})
        sys.exit(1)
