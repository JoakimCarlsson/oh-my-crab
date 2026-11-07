# Tool servers (MCP)

A tool server is a program or an HTTP address that offers tools over the Model
Context Protocol. Crab gives the agent the tools of the servers the client
passes in the `tools` list of `Open`, besides its own. It reads no tool-server
configuration of its own: not from the project, not from the user's folders,
and not from Claude Code's settings or plugins. A client that wants the
servers a user set up for another program reads that program's files and
passes them.

Each server has a `name` and a `reach`, which is either a `Command` crab
starts and speaks to on its standard input and output, or an `Http` address:

```json
{"Open":{"directory":"/work/app","provider":"claude","model":null,"yolo":false,
 "tools":[
  {"name":"github","reach":{"Command":{"program":"github-mcp-server","args":["stdio"],
    "env":[["GITHUB_PERSONAL_ACCESS_TOKEN","ghp_..."]]}}},
  {"name":"docs","reach":{"Http":{"url":"https://docs.example.com/mcp",
    "headers":[["Authorization","Bearer ..."]]}}}
 ],
 "denied":["mcp__github__delete_repository"]}}
```

`args`, `env` and `headers` must be present, empty lists when there are none.
A `Command` is started with crab's own environment plus `env`, in the folder
crab itself runs in, not the conversation's folder; what it writes to standard
error is discarded. Messages are one JSON object per line, and one request to
a program is answered before the next is sent. An `Http` server gets every
message as a POST with `headers`, and may answer with a JSON document or an
event stream; the `mcp-session-id` it hands out is sent back on later
requests. Crab does not open a stream for messages the server starts itself.
Crab speaks protocol version `2025-06-18` and introduces itself as `crab`.

The servers are reached when the conversation opens, all at once, and each has
20 seconds to answer the handshake and list its tools. A server that cannot be
started or reached, answers with an error or is too slow is left out and the
conversation opens without its tools; crab writes `tool server unavailable:
<name>: <reason>` to its standard error and the client gets no event for it.
Servers are not reached again during the conversation: once a program has
stopped, each call to its tools fails with `the tool server stopped`, and an
address that stops answering fails each call with the error. Programs
are stopped when another conversation is opened in the same process or crab
exits.

## How the model sees them

A tool is offered as `mcp__<server>__<tool>`, every character other than ASCII
letters, digits and `-` made `_`, and cut to 64 bytes, the most providers
take: the tool `get.file` of the server `my docs` is `mcp__my_docs__get_file`.
Its description and argument schema are the server's, unchanged. When two
tools end up with the same name, the later one, by the order of `tools` and
then of the server's list, wins. The tools are
listed with crab's own, by name, and the list is fixed for the conversation.
`denied` takes these names to keep a tool from the agent. Hooks match them by
the same name, such as `mcp__github__*`, and subagents and scripts can call
them like any other tool. `crab tools` prints only crab's own tools.

Arguments are put right against the tool's schema before the call, as for
crab's own tools (see [How tool calls run](../tools/calls.md)). The result is
the `text` parts of the server's answer joined by newlines; images and other
kinds of content are dropped. When the server marks the result `isError`, or
answers with a JSON-RPC error, the call fails and the model reads the text or
the error's message as the failure.

## Approvals and limits

Every call to a tool-server tool asks the user first, unless the conversation
is in yolo mode or a `PreToolUse` hook allows it. An "always allow" answer
covers later calls of the same tool, and no other, for as long as that `crab`
process runs. Tool-server tools count as tools that change things, so plan mode
refuses them.

Text longer than 20 KB is cut to its first and last 10 KB around a note of what
was cut, and the whole text is saved to a file the result names, which the
model can read in parts (see [How tool calls run](../tools/calls.md)). A
failure's text is cut the same way. A call has no time limit of its own;
cancelling the turn stops waiting for it.
