# Scripts

The `script` tool lets the model write a short JavaScript program that calls
its other tools, so that results it only needs to filter, count or combine
never enter its context: only what the program prints and returns does. The
program is the body of an async function. It calls a tool with
`await tools.grep({ pattern: "..." })`, with the same arguments as a direct
call, prints with `print` or `console.log`, and returns a value, which comes
back as indented JSON. Most tools resolve to their output text; `grep` and
`glob` resolve to their matches and paths as data. A call that does not
succeed rejects with a `ToolError` the program can catch, and an uncaught error
fails the script with what it printed before. `Promise.all` runs calls that
only look at once, while a call that changes something, or that needs
approval, waits for the calls started before it and runs alone (see
[which calls run at once](calls.md#which-calls-run-at-once)).
The tool's description lists the tools a script can call with their arguments
as TypeScript-like signatures, made from their schemas in name order, so the
description stays the same from one request to the next. [Hidden
tools](README.md#hidden-tools) are named without signatures; a script calls
them the same way once the model has loaded their parameters.

The program runs in QuickJS, embedded through `rquickjs`, on a thread of its
own. It has no file system, network, process, timer or module access: `tools`,
`print`, `console` and `ToolError` are all it can reach beyond the language
itself. A script runs for at most 60 seconds, or the `timeout` it is given up
to 300, not counting time spent waiting for the reader to approve one of its
calls; it may allocate 64 MiB and make 500 tool calls. A script past its time is
stopped even in a loop that calls nothing, and so is a script whose turn is
cancelled, along with the calls it is waiting on. Output is cut like a tool
server's, to its first and last 10 KB, with the whole saved to a file the
model can `read`.

Every call a script makes goes the way the model's own calls go: its arguments
are put right against the tool's schema or turned away, the hooks see it before
and after, under the tool's own name, and a call that needs approval asks the
reader, one call at a time, with the summary ending in `(called by script)`;
an "always allow" answer covers direct calls and scripted ones alike. A hook's
note after a call reaches the script in the call's text. Each call is reported
as a child of the `script` call: its tool events carry the script call's id as
`parent`, `script` as `agent`, and an id made of the script call's id and the
call's number, as the work of a subagent is reported under its `task` call.

Scripts never get `script` itself, `task`, `ask`, `exit_plan_mode`, `todo`,
`context_notes`, `new_context`, `checkpoint`, `rewind`, `wait`, `set_title`,
`submit_result`, `recall`, `skill`, `remember`, `memory_edit` or `load_tools`: the tools that
start subagents, talk to the reader, steer the conversation or only make sense
to the model. A denied tool is not offered to scripts either. In plan mode a
script can only call the tools that only look. Subagents whose kind does not
list its tools get `script` too, calling their own tools, and its calls are
reported under the script and then under the subagent.
