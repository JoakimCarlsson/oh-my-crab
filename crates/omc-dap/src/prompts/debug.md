Drive a real debugger (lldb-dap, gdb, Delve, debugpy) through the Debug Adapter Protocol. Use it to find out why a program crashes, hangs or computes a wrong value by stopping it and looking at its state, instead of adding print statements.

A session lasts across calls until you disconnect it or the conversation ends; actions act on the session used last unless you pass `session`. Build native programs with debug info first (`gcc -g -O0`, `cargo build`, `go build -gcflags=all="-N -l"`).

Actions:
- `launch`: start `program` (a file or folder in the working directory) with `args`, `cwd` and `env`. The debugger is chosen from the program (compiled binary, `.py`, `.go` or Go package folder) or from `language`/`adapter`; `config` adds adapter-specific launch settings. Pass `breakpoints` ("file:line" or a function name) to set them before the program runs, and `stop_on_entry` to stop at `main`. Returns where it stopped, that it exited, or that it is still running.
- `attach`: attach to a running process by `pid`, or connect to a debug adapter already listening at `address` ("host:port").
- `break`: set a breakpoint at `file` and `line`, or on `function`, with optional `condition`, `hit_condition` (such as "3" or ">= 3") or `log_message` (a logpoint that prints "x is {x}" instead of stopping). `clear` removes one (file and line, file alone, function) or `all`. `breakpoints` lists them. `exceptions` sets the exception filters (`filters`; an empty list turns them off) and lists the ones the adapter has.
- `continue`, `step_over`, `step_in`, `step_out`: run the stopped program until it stops again, or until `timeout` seconds (default 15) pass and it is reported as still running. `pause` interrupts it; `wait` waits for the next stop. A stop that happens between calls is reported on your next call, and `continue` then reports it instead of running on.
- `threads`, `stack` (`thread`, `count`), `scopes` (`frame`): where the stopped program is. Frame numbers count from 0, the innermost.
- `variables`: the variables of `frame` (all cheap scopes, or just `scope`), or the children of `ref` (shown as `[ref N]` beside values that have children), expanded `depth` levels (default 1) with at most `limit` children each (default 50).
- `evaluate`: evaluate `expression` in `frame` as a watch expression in the program's language; results with children show a `[ref N]`.
- `output`: what the program printed since you last saw it, or everything kept with `all`.
- `sessions`: list the open sessions. `disconnect`: end a session; a launched program is ended, an attached one left running unless `terminate` says otherwise.

Stops report the reason, the source line, the top frames and the local variables, so you rarely need `stack` and `variables` right after one. Disconnect sessions you are done with.
