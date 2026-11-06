<memory>
{{#if memories}}Memories saved by earlier conversations in this project, or kept for the user across projects (marked `user`), most relevant to this prompt first. Each was true when it was saved and may have gone stale since: check one against the code before relying on it.{{#if has_memory_edit}} Correct or forget a wrong one with `memory_edit`.{{/if}}
{{memories}}{{else}}No memories are saved for this project yet.{{/if}}
</memory>
