Run a short JavaScript program that calls your other tools; only what it prints and returns comes back to you. Intermediate results never enter your context, so use it when many calls feed one small answer: scanning many files, filtering or counting matches, reading only the ranges that matter, cross-checking results. Reach for it from the start rather than after listing or searching directly, do the filtering and aggregating inside it, and return only the answer: returning raw per-item data defeats the purpose. For one or two calls whose output you need to see, call the tools directly.

`code` is the body of an async function. Call a tool with `await tools.<name>(args)`, passing the same arguments as a direct call; it resolves to the tool's output text, or to the structured value its signature shows. A call that does not succeed (bad arguments, denied, blocked by a hook, failed) rejects with a `ToolError` (`.message`, `.tool`) you can catch. `Promise.all` runs calls that only look at once, while a call that changes something waits for the calls before it. `print(...)`/`console.log(...)` add a line to the output; `return` a value to get it back as JSON. Every call is checked, approved and shown to the user like your own calls. Start the code with a `// comment` saying what it does; the user sees it.

Nothing else is reachable: no `require`, `import`, `fetch`, `process`, files, network or timers. Limits: {{seconds}} s of wall time unless `timeout` raises it (at most {{most_seconds}} s), {{memory}} MiB of memory, {{calls}} tool calls. Output over {{output}} KiB is cut, and saved whole to a file you can `read`.

Example, grep many files and read only the lines around the first match in each:
```js
// Show where parse_args is called, a few lines around each caller
const { matches } = await tools.grep({ pattern: "parse_args\\(", glob: "*.rs", limit: 500 });
const first = new Map();
for (const m of matches) if (!first.has(m.path)) first.set(m.path, m.line);
const out = {};
for (const [path, line] of first) {
  out[path] = await tools.read({ path: `${path}:${Math.max(1, line - 3)}-${line + 5}` });
}
return out;
```

Example, count without reading anything yourself:
```js
// Count TODOs per top-level folder
const { matches, truncated } = await tools.grep({ pattern: "TODO|FIXME", limit: 500 });
const counts = {};
for (const m of matches) {
  const top = m.path.split("/")[0];
  counts[top] = (counts[top] ?? 0) + 1;
}
for (const [folder, n] of Object.entries(counts).sort((a, b) => b[1] - a[1])) print(folder, n);
if (truncated) print("stopped at 500 matches; narrow the search for exact counts");
```

Tools:
{{tools}}
