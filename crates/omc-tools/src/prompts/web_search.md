Search the web. Returns ranked results, each with its title, URL, date when known and a snippet, and a short written answer when the search service gives one. Use it for anything outside the working directory: documentation, error messages, releases, APIs, news. Then `read` the URLs worth reading in full; never trust a snippet for details.
- `query`: words as you would type them into a search engine; `site:docs.rs` and quoted phrases work with most services.
- `limit`: 1 to 20 results, 8 by default.
- `recency`: `day`, `week`, `month` or `year` for recent things.
The header names the service that answered and any that failed before it. The user approves searches, since the query leaves the machine.
