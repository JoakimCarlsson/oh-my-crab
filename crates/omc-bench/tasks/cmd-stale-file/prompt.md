Do these steps in this order:

1. Read `config/app.conf`.
2. Run `python3 tools/normalize.py config/app.conf`. It rewrites the file: keys in lower case, each section's keys sorted, one blank line between sections.
3. In the rewritten file, change `timeout` in the `[server]` section to `45`, and add the line `retries = 3` to the `[client]` section, in its sorted place among that section's keys.

Leave everything else in the rewritten file as the script left it, and do not change `tools/normalize.py`.
