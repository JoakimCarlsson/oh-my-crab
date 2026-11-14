"""The Python kernel of the eval tool.

Reads one JSON request per line on standard input, runs its code in one
namespace kept for the life of the process, and answers with one line that
starts with the marker in OMC_EVAL_MARKER, followed by JSON holding
the captured output, the value of a final expression and any error.
"""

import ast
import contextlib
import io
import json
import os
import sys
import traceback

MARKER = os.environ["OMC_EVAL_MARKER"]
NAMESPACE = {"__name__": "__main__", "__builtins__": __builtins__}


def run(code):
    """Run code in the shared namespace.

    Args:
        code: Python source; a final expression statement is evaluated and its
            repr returned, as an interactive interpreter would show it.

    Returns:
        A tuple of the repr of the final expression, or None, and the
        formatted traceback of an exception, or None.
    """
    try:
        tree = ast.parse(code, "<eval>", "exec")
        last = None
        if tree.body and isinstance(tree.body[-1], ast.Expr):
            last = ast.Expression(tree.body.pop().value)
        exec(compile(tree, "<eval>", "exec"), NAMESPACE)
        if last is not None:
            value = eval(compile(last, "<eval>", "eval"), NAMESPACE)
            if value is not None:
                NAMESPACE["_"] = value
                return repr(value), None
        return None, None
    except BaseException:
        kind, error, trace = sys.exc_info()
        frames = traceback.extract_tb(trace)
        kept = [frame for frame in frames if frame.filename == "<eval>"]
        lines = traceback.format_list(kept) + traceback.format_exception_only(kind, error)
        return None, "Traceback (most recent call last):\n" + "".join(lines)


def serve():
    """Answer requests on standard input until it closes."""
    for line in sys.stdin:
        request = json.loads(line)
        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer), contextlib.redirect_stderr(buffer):
            value, error = run(request["code"])
        sys.stdout.write(
            MARKER
            + json.dumps({"output": buffer.getvalue(), "value": value, "error": error})
            + "\n"
        )
        sys.stdout.flush()


serve()
