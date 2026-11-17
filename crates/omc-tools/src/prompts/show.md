Puts a visual in front of the user: a diagram or a small prototype, shown beside the conversation. It does not wait for the user; go on with the work, or use `ask` when you need their decision.

Use it when a picture explains better than prose: an architecture, a flow, a state machine or a sequence as a diagram; a layout, a component or a page as a prototype. Do not use it for what reads fine as text or code.

<instruction>
- `kind: "mermaid"`: `content` is Mermaid source alone, no code fence.
- `kind: "html"`: `content` is one complete, self-contained HTML document with inline CSS. It is rendered sandboxed with the network denied: NEVER reference external scripts, stylesheets, fonts or images.
- `title`: a few words naming what is shown.
- Keep it small and focused, well under 256 KiB; show a new version with a new call.
</instruction>
