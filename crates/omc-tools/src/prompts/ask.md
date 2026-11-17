Asks the user questions during execution and waits for the answers.

Ask only for materially different tradeoffs the user must decide. Default: act using code/config/docs/history and conventions. Several viable choices: pick conservative/standard, proceed, state choice.

<instruction>
- Batch related questions; 2-5 distinct options each; short labels, tradeoffs in `description`.
- `recommended` (0-based index) auto-adds " (Recommended)"; `multi: true` permits multiple selections.
- NEVER supply "Other": the UI adds "Other (type your own)". Clarifying custom input? Answer first; re-ask unresolved questions.
- Visual choice (layouts, components, styles)? Give each option a `mockup`: a small complete HTML document with inline styles picturing it, shown side by side for comparison. No scripts needed; NEVER reference external resources, the network is denied. Skip mockups for non-visual choices.
- Blocks immediately: MUST first emit unseen decision context (findings, diffs, drafts) as assistant text, even when batching `ask` with other tools.
</instruction>
