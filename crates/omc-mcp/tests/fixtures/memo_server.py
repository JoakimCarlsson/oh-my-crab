"""A small MCP tool server over standard streams, for tests: two tools and some resources."""

import json
import os
import sys

MEMOS = {"greeting": "Hello from the memo server.", "pid": str(os.getpid())}


def result(message_id, value):
    """Writes the result `value` of the request `message_id`."""
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": value}) + "\n")
    sys.stdout.flush()


def handle(message):
    """Answers one message read from standard input."""
    method = message.get("method")
    message_id = message.get("id")
    if message_id is None:
        return
    params = message.get("params") or {}
    if method == "initialize":
        result(message_id, {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "memo", "version": "1"},
        })
    elif method == "tools/list":
        result(message_id, {"tools": [
            {"name": "add", "description": "Adds two numbers.",
             "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
                             "required": ["a", "b"]},
             "annotations": {"readOnlyHint": True}},
            {"name": "crash", "description": "Stops the server.", "inputSchema": {"type": "object"}},
        ]})
    elif method == "tools/call":
        arguments = params.get("arguments") or {}
        if params.get("name") == "crash":
            sys.exit(3)
        total = arguments.get("a", 0) + arguments.get("b", 0)
        result(message_id, {"content": [{"type": "text", "text": f"{total:g}"}]})
    elif method == "resources/list":
        result(message_id, {"resources": [
            {"uri": "memo://greeting", "name": "greeting", "mimeType": "text/plain", "description": "A greeting."},
            {"uri": "memo://pid", "name": "pid", "mimeType": "text/plain", "description": "The server's process id."},
        ]})
    elif method == "resources/templates/list":
        result(message_id, {"resourceTemplates": [
            {"uriTemplate": "memo://{name}", "name": "memo", "description": "Any memo by name."},
        ]})
    elif method == "resources/read":
        uri = params.get("uri", "")
        name = uri.removeprefix("memo://")
        if name not in MEMOS:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id,
                                         "error": {"code": -32002, "message": f"no memo {name}"}}) + "\n")
            sys.stdout.flush()
            return
        result(message_id, {"contents": [{"uri": uri, "mimeType": "text/plain", "text": MEMOS[name]}]})
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id,
                                     "error": {"code": -32601, "message": "method not found"}}) + "\n")
        sys.stdout.flush()


for line in sys.stdin:
    if line.strip():
        handle(json.loads(line))
