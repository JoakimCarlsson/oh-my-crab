#!/usr/bin/env python3
"""A small crab extension: guards shell commands and gives the model a tool.

It speaks crab's extension protocol, JSON-RPC 2.0 messages one per line on
standard input and output, with nothing but the standard library:

- it refuses `bash` calls that run `rm`;
- it rewrites `bash` calls that run `date` to run with `TZ=UTC`;
- it gives the model a `count_words` tool;
- it tells the model, before each prompt, that the guard is on;
- it logs every event it is told to standard error, and to the file
  `GUARD_LOG` names when that is set, one JSON object per line.

`GUARD_CRASH` set to an event name makes it exit when told that event, and
`GUARD_SLOW` set to a number of seconds makes it wait that long before it
answers `tool_call`, so that tests can see crab restart it and time it out.
"""

import json
import os
import re
import sys
import time

EVENTS = [
    "session_start",
    "turn_start",
    "message",
    "tool_call",
    "tool_result",
    "turn_end",
    "session_end",
]

TOOLS = [
    {
        "name": "count_words",
        "description": "Counts the words in a text, as the guard extension sees them.",
        "input_schema": {
            "type": "object",
            "properties": {"text": {"type": "string", "description": "The text to count."}},
            "required": ["text"],
        },
        "read_only": True,
    }
]


def send(message):
    """Writes one JSON-RPC message to standard output.

    Args:
        message: The message object.
    """
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def log(entry):
    """Records `entry` on standard error and in the file `GUARD_LOG` names.

    Args:
        entry: A JSON-serialisable object describing what happened.
    """
    line = json.dumps(entry)
    sys.stderr.write(line + "\n")
    sys.stderr.flush()
    path = os.environ.get("GUARD_LOG")
    if path:
        with open(path, "a", encoding="utf-8") as file:
            file.write(line + "\n")


def on_event(params):
    """Answers one event crab told or asked about.

    Args:
        params: The event's parameters, with `event` naming it.

    Returns:
        The answer object, or None for no opinion.
    """
    event = params.get("event")
    log({"event": event, "tool": params.get("tool_name"), "input": params.get("tool_input")})
    if os.environ.get("GUARD_CRASH") == event:
        sys.exit(3)
    if event == "turn_start":
        return {"context": "The guard extension is watching shell commands."}
    if event == "tool_call" and params.get("tool_name") == "bash":
        slow = os.environ.get("GUARD_SLOW")
        if slow:
            time.sleep(float(slow))
        command = params.get("tool_input", {}).get("command", "")
        if re.search(r"(^|[;&|\s])rm\s", command):
            return {"block": True, "reason": "the guard extension does not allow rm"}
        if re.match(r"^\s*date\b", command):
            rewritten = dict(params["tool_input"])
            rewritten["command"] = "TZ=UTC " + command.strip()
            return {"input": rewritten, "context": "the guard extension pinned date to UTC"}
    return None


def on_tool(params):
    """Runs one of the tools this extension gives.

    Args:
        params: The call's `name`, `call_id` and `arguments`.

    Returns:
        The result object with `content`, and `is_error` for a failure.
    """
    log({"tool_call": params.get("name"), "arguments": params.get("arguments")})
    if params.get("name") != "count_words":
        return {"content": "unknown tool", "is_error": True}
    words = len(str(params.get("arguments", {}).get("text", "")).split())
    return {"content": f"{words} words"}


def main():
    """Reads requests from standard input until it closes or crab says `shutdown`."""
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        method = message.get("method")
        params = message.get("params") or {}
        if method == "shutdown":
            break
        if method == "initialize":
            result = {"events": EVENTS, "tools": TOOLS}
        elif method == "event":
            result = on_event(params)
        elif method == "tool":
            result = on_tool(params)
        else:
            result = None
        if "id" in message:
            send({"jsonrpc": "2.0", "id": message["id"], "result": result})


if __name__ == "__main__":
    main()
