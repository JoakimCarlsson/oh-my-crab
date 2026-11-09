# Tool servers (MCP)

A tool server is a program or an HTTP address that offers tools and resources
over the Model Context Protocol. Crab gives the agent the tools of the servers
the client passes in the `tools` list of `Open`, and of the servers set up in
files: crab's own `mcp.json`, the project's `.mcp.json` once the project is
trusted, and, when asked for, Claude Code's `~/.claude.json`.

## Servers the client passes

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
A `Command` the client passes is started with crab's own environment plus
`env`, in the folder crab itself runs in, not the conversation's folder.

## Servers set up in files

Every file has the shape of Claude Code's `.mcp.json`: each server under
`mcpServers` by name, either a program or an address.

```json
{"mcpServers":{
  "memo":  {"command":"memo-mcp","args":["--root","."],"env":{"LEVEL":"warn"}},
  "issues":{"type":"http","url":"https://issues.example.com/mcp",
            "headers":{"X-Team":"${TEAM:-core}"}},
  "legacy":{"type":"sse","url":"https://old.example.com/sse"},
  "wiki":  {"type":"http","url":"https://wiki.example.com/mcp",
            "oauth":{"clientId":"crab-at-example","callbackPort":8765,"scopes":["read"]}},
  "off":   {"command":"unused-mcp","disabled":true}
}}
```

- A program has a `command`, and optionally `args`, `env` and `cwd` (a folder
  relative to the one it starts in). `type` may be left out or be `stdio`.
- An address has a `url`, optionally `headers`, and a `type` of `http` (the
  default; `streamable-http` is read the same) or `sse` for the older
  transport of one long event stream.
- `"disabled": true` or `"enabled": false` leaves a server out.
- `oauth` is only needed for a server whose authorization server does not
  register clients itself: `clientId`, `clientSecret` for a confidential
  client, `callbackPort` for a client registered with a fixed address, and
  `scopes` (a list or one string) to ask for instead of those the server
  suggests. See [Signing in](#signing-in).
- In `command`, `args`, `env` values, `cwd`, `url`, `headers` values and the
  `oauth` texts, `${NAME}` is replaced by the environment variable `NAME`, and
  `${NAME:-fallback}` by the variable or, when it is unset or empty, the
  fallback. An unset variable without a fallback becomes empty text. A `$`
  that starts neither form stays as it is.

Fields crab does not know are ignored, so one file can serve several programs.
A file that is not JSON, or an entry of the wrong shape, is reported on
standard error as `tool servers: <file>: ...` and skipped.

The files read, and which wins when two name the same server, from the first
that wins:

1. The servers the client passed in `Open`.
2. The project's files, from the working folder up to the repository root
   (only the working folder outside a repository), the nearer folder first,
   and in each folder `.oh-my-crab/mcp.json` before `.mcp.json`. Read only once
   the project is trusted; see [Trust](#trust).
3. With `OMC_MCP_CLAUDE=1`, the servers Claude Code's `~/.claude.json` keeps
   for this project (under `projects`, by the working folder and then the
   repository root).
4. Crab's own `<config>/mcp.json`, or the file `OMC_MCP_FILE` names.
5. With `OMC_MCP_CLAUDE=1`, the servers `~/.claude.json` keeps for every
   project, its top-level `mcpServers`. `~/.claude.json` is
   `$CLAUDE_CONFIG_DIR/.claude.json` when `CLAUDE_CONFIG_DIR` is set.

A server is taken whole from the file that wins: nothing of a losing entry,
such as its headers, is merged in. `OMC_MCP_CONFIG=0` reads none of these
files, leaving only the servers the client passes. Claude Code's file is not
read unless asked for, since it holds servers set up for another program that
would otherwise start in every conversation, for every client.

A project's program starts in the project folder that holds the file (or in
its `cwd`); a program of the user's files starts in the conversation's folder.
Both get crab's own environment plus their `env`. The user's files are read
when the conversation opens; the project's files are read again before every
prompt, so a server added to a trusted project is reached from the next turn,
once it is trusted again (see below).

## Trust

A cloned repository must not start programs, or send the user's environment
variables to an address of its choosing, before the user trusts it. So the
servers of a project's files wait for [project trust](project-trust.md) as its
hooks do: while the project is not trusted, or a hook or server was added or
changed since it was trusted, none of them is started, and each is listed in
the `Trust` event as a `Hook` whose name is `tool server <name>: <command line
or address>`, as the file writes it, before variables are replaced. Trusting
the project reaches them before the next turn. Trust records them with the
project's hooks, so changing a server's command or address asks again.

## How servers are reached

The servers are reached when the conversation opens, all at once, and each has
20 seconds to answer the handshake and list its tools. A server that cannot be
started or reached, answers with an error or is too slow is left out and the
conversation opens without its tools; crab writes `tool server unavailable:
<name>: <reason>` to its standard error and the client gets no event for it. A
server that is left out is not tried again in that conversation, except one
waiting for a sign-in, whose tools arrive once the reader signs in.

Messages to a program are one JSON object per line, and one request is
answered before the next is sent; what it writes to standard error is
discarded. A `ping` it sends is answered, and other requests it sends are
refused, since crab offers it no capabilities. When a program stops, the call
it stopped in fails with `the tool server stopped`, and the next call starts
it again with a new handshake; a call is never sent twice, since part of it
may have been done.

An `http` address gets every message as a POST, and may answer with a JSON
document or an event stream; the `mcp-session-id` it hands out is sent back on
later requests. An `sse` address is the older transport: crab opens its event
stream, posts each message to the endpoint the stream names, and reads the
answers from the stream. Crab does not open a stream for messages the server
starts itself on an `http` address. Crab speaks protocol version `2025-06-18`
and introduces itself as `crab`.

Programs are stopped when another conversation is opened in the same process
or crab exits.

## Signing in

An address that answers `401` wants a token. Unless the server's headers
carry their own `Authorization`, crab signs in as the MCP authorization spec
describes:

1. It reads the server's protected-resource metadata, from the address the
   `WWW-Authenticate` header names or from
   `/.well-known/oauth-protected-resource`, and from it the authorization
   server, or takes the server's own origin when there is none.
2. It reads the authorization server's metadata
   (`/.well-known/oauth-authorization-server`, then OpenID's
   `/.well-known/openid-configuration`), or uses `/authorize`, `/token` and
   `/register` on its origin when it publishes none. Metadata that names
   another issuer, or does not support PKCE with `S256`, is refused.
3. It registers itself as a public client named `crab`, unless the file gives
   `oauth.clientId`.
4. It listens on `127.0.0.1`, on `oauth.callbackPort` or a free port, and tells
   the client the sign-in page as a `LoginUrl` event, also writing `tool server
   <name> wants signing in: <url>` to standard error. The page asks for an
   authorization code with PKCE (`S256`), a `state`, the server's address as
   `resource`, and the scopes the challenge, the metadata or `oauth.scopes`
   name.
5. When the browser comes back to `/callback` with the code, crab exchanges it
   for tokens, keeps them, connects to the server, and gives the agent its
   tools from the next prompt on.

The client opens the page; crab does not. The sign-in waits ten minutes for
the browser and the conversation goes on meanwhile; the server is reported
unavailable, `waiting for the reader to sign in`, until then. When the
sign-in fails, crab writes `tool server sign-in failed: <name>: <reason>` to
standard error.

Tokens are kept in `mcp-auth.json` in `<config>`, or in the file
`OMC_MCP_AUTH_FILE` names, shared by every conversation whatever `home` its
provider login is kept in, by the server's address, readable by the user alone
on systems with file modes. A kept token is sent with every request to its
server; one that has expired is refreshed first, and one the server turns away
is refreshed and the request sent once more. A token that cannot be refreshed
is forgotten, and the next conversation signs in again. Sign-ins work for the
servers the client passes too, since the client cannot sign in for crab.

## How the model sees them

A tool is offered as `mcp__<server>__<tool>`, every character other than ASCII
letters, digits and `-` made `_`, and cut to 64 bytes, the most providers
take: the tool `get.file` of the server `my docs` is `mcp__my_docs__get_file`.
Its description and argument schema are the server's, unchanged. When two
tools end up with the same name, the later one wins. The tools are listed
with crab's own, by name; servers reached after the conversation opened, by
trust or a sign-in, add theirs from the next turn on. `denied` takes these
names to keep a tool from the agent. Hooks match them by the same name, such
as `mcp__github__*`, and subagents and scripts can call them like any other
tool. `crab tools` prints only crab's own tools.

Arguments are put right against the tool's schema before the call, as for
crab's own tools (see [How tool calls run](../tools/calls.md)). The result is
the `text` parts of the server's answer joined by newlines; images and other
kinds of content are dropped. When the server marks the result `isError`, or
answers with a JSON-RPC error, the call fails and the model reads the text or
the error's message as the failure.

### Resources

When a server says it offers resources, the agent also gets one tool for all
such servers, `mcp_resources`:

- Without `uri`, it lists the resources (`resources/list`) and resource
  templates (`resources/templates/list`) of every such server, or of
  `server`, one a line: `<server>: <uri> (<name>) [<mime type>] - <first line of
  the description>`, and `<server>: template <uri template> ...` for
  templates.
- With `uri`, it reads the resource (`resources/read`) from `server`, or from
  the first server that can read it. Text parts are shown whole; a binary part
  is shown as a note of its type and size.

`mcp_resources` only reads, so it never asks first and plan mode allows it.
Its output is cut like a tool's. A server that offers resources and no tools
is still reached for its resources. Prompts of servers are not offered.

## Approvals and limits

Every call to a tool-server tool asks the user first, unless the conversation
is in yolo mode, a `PreToolUse` hook allows it or a permission rule allows it.
An "always allow" answer adds a rule for the tool, such as
`mcp__github__get_issue`, which covers later calls of the same tool and no
other, for the rest of the conversation or, kept, for later ones too. Rules
can name a whole server, `mcp__github`, or a pattern, `mcp__github__get_*`;
a deny rule stops a tool in yolo mode too. See
[permission rules](permissions.md). Tool-server tools count as tools that change things, so plan mode
refuses them.

A tool-server tool runs alone among the calls of a reply, unless its server
marks it with the `readOnlyHint` annotation: then its calls may overlap the
other calls that only look. The hint changes nothing else; see
[which calls run at once](../tools/calls.md#which-calls-run-at-once).

Text longer than 20 KB is cut to its first and last 10 KB around a note of what
was cut, and the whole text is saved to a file the result names, which the
model can read in parts (see [How tool calls run](../tools/calls.md)). A
failure's text is cut the same way. A call has no time limit of its own;
cancelling the turn stops waiting for it.
