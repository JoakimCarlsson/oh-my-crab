# Debugging

The `debug` tool drives real debuggers through the Debug Adapter Protocol, so
the agent can find why a program crashes, hangs or computes a wrong value by
stopping it and looking, rather than by adding print statements. One tool takes
an `action`: `launch` a program with its arguments, folder and environment, or
`attach` to a process by id or to an adapter already listening at
`host:port`; set and clear breakpoints at a line, with a condition, a hit count
or a log message instead of a stop, on a function, or on exceptions;
`continue`, `pause` and step over, into or out; list the threads, the stack and
a frame's scopes; read variables, expanded to a few levels with a bounded
number of children each; `evaluate` an expression in a frame; read what the
program printed; and `disconnect`. Every answer is short text rather than the
protocol's JSON: a stop says why and where, shows the source line with the
lines around it, the top frames and the frame's arguments and locals, and what
the program printed since the last call.

The adapter comes from the program: a compiled program is debugged with
`lldb-dap`, `codelldb` or `gdb -i dap` (gdb 14 or newer), whichever is
installed first in that order, a `.py` file with `debugpy` (the project's
`.venv` or `venv` interpreter first, then `python3`, as long as it can import
`debugpy`), and a `.go` file or Go package folder with `dlv dap`. A call can
name the `language` or the `adapter`, and add adapter-specific settings to the
launch request with `config`. When nothing is installed for the language, the
error names what to install. `OMC_DAP_<ID>` replaces an adapter's command, as
in `OMC_DAP_LLDB_DAP=/opt/llvm/bin/lldb-dap` or
`OMC_DAP_DEBUGPY="/srv/venv/bin/python -m debugpy.adapter"`, and
`debuggers.json` in crab's [config folder](../configuration.md#folders)
(`~/.config/oh-my-crab/` on Linux), or the file `OMC_DEBUGGERS_FILE` names,
changes the known adapters and adds others:

```json
{
  "adapters": [
    { "id": "gdb", "command": ["/opt/gdb/bin/gdb", "-i", "dap"] },
    {
      "id": "zig-lldb",
      "command": ["/opt/zig-lldb/bin/lldb-dap"],
      "languages": ["zig"],
      "transport": "stdio",
      "launch": { "initCommands": [] }
    }
  ]
}
```

A new adapter needs `command` and `languages`, and is preferred over the known
ones for its languages. `transport` is `stdio`, or `port` for an adapter that
listens on the port written `{port}` in its command. `launch`, `binary`,
`attach` and `remote` are settings added to every launch, to launches of a
compiled program, to attaching to a process, and to attaching through an
address. Adapters that start child sessions of their own, as the JavaScript
debugger does, are not supported.

A session lasts across tool calls until the agent disconnects it, up to four at
once, named `dbg-1`, `dbg-2` and so on; an action without `session` acts on the
one used last. Running and stepping wait for the next stop at most `timeout`
seconds, 15 by default and 300 at most, and then answer that the program is
still running instead of holding the turn; `wait` waits again and `pause`
interrupts it. A stop or an exit that happens between calls is reported at the
top of the next call on that session, and a `continue` or step then reports it
instead of running on. Every request to the adapter has a time limit too, so an
adapter that stops answering fails the call rather than hanging it. Sessions
belong to the conversation, like background runs: a cancelled turn leaves them
as they are, and when the conversation closes or another is opened each
adapter is told to disconnect, which ends the programs it launched and detaches
from the ones it attached to; whatever is left after a few seconds is killed,
together with the adapter's process group and any program a session launched.

Launching and attaching run code, so outside `yolo` mode they ask for approval
like `bash`, under the scope `debug`; the other actions work on a session the
user already allowed and ask nothing. The program, its folder and the files of
line breakpoints must be inside the working directory, and source lines are
shown only from files inside it. Expressions are evaluated as watch
expressions, never as debugger commands. `debug` changes the program's state,
so a call to it in plan mode is refused, as for every tool that changes things
(see [long conversations](../long-conversations.md)), and subagents do not get it. Its tool events
are of kind `Execute` when they launch, attach, continue or step, and `Other`
otherwise.
