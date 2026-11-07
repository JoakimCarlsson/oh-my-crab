# Project trust

A freshly cloned repository can steer the agent, so what a project folder
brings is not used until the reader trusts the project: its hooks, its
instruction files (`AGENTS.md`, `CLAUDE.md`, `.claude/rules` and the like),
its skills and agent definitions, and the Claude Code plugins its settings
switch on or off. The user's own folders (`~/.claude`, `~/.codex`,
`~/.agents/skills` and crab's config folder) and the plugins the user
installed are always used, and a project that is the user's home folder is the
user's own. Crab reads no tool-server configuration from a project; the tool
servers are the ones the client passes when it opens the conversation. In
yolo mode the language servers of an untrusted project start on the first
write instead of when the conversation opens.

Trust is recorded per repository: the main checkout and every linked worktree
of it share one decision, while another repository with the same folder name
does not. Outside a repository the project is the folder. Decisions are kept
in `trust.json` under `oh-my-crab` in the user's data folder, or in the file
`OMC_TRUST_FILE` names, with the project's hook commands as they were when it
was trusted. When a trusted project later has a hook that is not among them,
added or changed, its hooks are left out until it is trusted again, while its
instruction files, skills and agent definitions stay in use; a hook that is
removed changes nothing.

When something is left out, crab tells the client with a `Trust` event: the
folder trust is recorded for, the standing (`Unknown`, `Changed` or
`Declined`), and each input left out with its kind, name and file. The client
answers with a `Trust` command under the event's ticket: `Always` trusts the
project for good, `Allow` for this conversation only, and `Reject` records
that it is not trusted, which is not asked about again in that conversation.
What a trusted project brings is used from the next turn on. The event is sent
when the conversation opens and again when what is left out changes, since the
files are read again before every prompt. `crab trust <folder> on` and `off`
record the same decisions from a shell, and `crab hooks [folder]` prints every
hook found, whether it runs, and what is left out. Set `OMC_TRUST_ALL=1` to
trust every project for the conversations of the process without recording
anything, for automation that vouches for what it runs in.
