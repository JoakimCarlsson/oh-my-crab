Run Python or JavaScript code in a kernel that keeps its state between calls: variables, functions and imports defined in one call are there in the next. Use it to compute, explore data or try an API step by step; use `bash` for shell commands.
- `language`: `python` (python3) or `javascript` (Node.js). Each has its own kernel, started in the working directory on first use.
- `code`: the code to run. Printed output is returned, then the value of a final expression, as an interactive interpreter shows it. JavaScript may use top-level `await`, but then declarations stay local to that call; assign to `globalThis` to keep them.
- `reset`: throw the kernel's state away first; with no `code`, only resets.
- `timeout`: seconds, 60 by default, at most 600. A call that runs past it stops the kernel and loses its state.
Output past 50 KB keeps its start and end. An exception fails the call with its traceback; the state from before it stays.
