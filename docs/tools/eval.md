# eval

`eval` runs Python or JavaScript code in a kernel that keeps its state
between calls, so that variables, functions and imports defined in one call
are there in the next.

## Arguments

| Argument   | Meaning                                                                          |
| ---------- | -------------------------------------------------------------------------------- |
| `language` | `python` or `javascript` (`js` and `node` are accepted too). Required.           |
| `code`     | The code to run. May be left out when `reset` is true.                           |
| `reset`    | Throw the kernel's state away before running the code. Defaults to false.        |
| `timeout`  | Seconds to wait, 60 by default, between 1 and 600.                               |

## Kernels

Each language has one kernel: an interpreter process started in the working
folder on the first call and kept until the conversation ends. Python runs
under `python3` and JavaScript under `node`; `OMC_EVAL_PYTHON` and
`OMC_EVAL_NODE` name other interpreters. Calls to one kernel run one at a
time.

The result is what the code printed, standard error included, followed by
the value of the final expression the way an interactive interpreter shows
it. JavaScript may use top-level `await`; code that does runs inside an async
function, so its declarations stay local unless assigned to `globalThis`.

An exception fails the call with its traceback, and the kernel keeps the
state it had. A call that runs past its timeout, or a kernel that exits,
stops the kernel and everything it started; its state is lost and the next
call starts a fresh one.

Output past 50 KB keeps its first and last 25 KB, with a line saying how much
was left out.

## Approval

Code can do anything, so every call asks outside yolo mode; an "always allow"
answer covers one language. Plan mode refuses the tool, and read-only
subagents do not get it.
