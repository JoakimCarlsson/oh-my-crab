# Reading the web and GitHub

`read` of an `http://` or `https://` URL fetches the page and returns it as
numbered lines, like a file. An HTML page becomes Markdown with its links and
images made absolute, so they can be read in turn. Only the page's main
content is kept: its `main` element, or its one `article`, without the
navigation, headers, footers, sidebars, cookie banners and hidden elements
around it; a page without either keeps its body, still without that chrome,
and a page where that would leave next to nothing is kept whole. The padding
that lines up table columns is taken out. `url:full` converts the whole page
and `url:raw` returns the HTML as sent. A PDF, such as an arXiv paper, gives
its text page by page, JSON is indented with its keys in the order sent
(`url:raw` keeps it as sent), and other text is shown as it is. Nothing on the
page runs: scripts are dropped unread. Line selectors work as they do for
files (`url:100-200`). A fetch takes at most 30 seconds and reads at most
10 MiB of the body. A page longer than one read shows is saved whole, in the
same folder as long `bash` output, and the note that says where to go on
points into that file, so the rest is paged from disk rather than fetched
again.

GitHub is read through the same tool, by URL or by a short path, without the
model needing other tools:

| Path                                             | Result                                                 |
| ------------------------------------------------ | ------------------------------------------------------ |
| `pr://123`, `issue://45`                         | in the repository the `origin` remote points at        |
| `pr://owner/repo/123`, `issue://owner/repo/45`   | in the repository named                                |
| `gh://owner/repo/pull/123`                       | state, description, changed files, reviews, comments   |
| `pr://123:diff`, `gh://owner/repo/pull/123:diff` | the unified diff; `:diff:1-400` pages it               |
| `gh://owner/repo/issues/45`                      | the issue and its comments                             |
| `gh://owner/repo/blob/<ref>/<path>`              | a file at a branch, tag or commit, with line selectors |
| `gh://owner/repo/tree/<ref>/<path>`              | a directory at a ref                                   |
| `gh://owner/repo`                                | the repository's facts, top-level files and README     |

`https://github.com/...` URLs of the same things, and `github.com/...` without
the scheme when no such file exists in the working directory, read the same
way; a pull request's `/files` page reads as its diff. A ref is the first
path segment after `blob` or `tree`, so a branch with a `/` in its name is
named by its commit instead. Other github.com pages
are fetched as web pages. The repository of `pr://` and `issue://` comes from
`git remote get-url origin` in SSH, `ssh://`, `git://` or HTTPS form; when
there is no such remote, or it is not on GitHub, the error says to name the
repository. GitHub is asked through its REST API, signed in with the token of
the `gh` command line tool when it is installed and signed in, otherwise with
`GH_TOKEN` or `GITHUB_TOKEN`, otherwise not at all, which reaches public
repositories at GitHub's lower anonymous rate limit. The token is kept in
memory for the rest of the process and forgotten when GitHub refuses it;
nothing is written to disk. Lists of comments, reviews and files show their
first 100 entries. The header of each result says how the request was signed.

Reading the network asks first unless the conversation is in yolo mode, as
`bash` does. The approval is per site: "always allow" adds a rule for the
host, such as `WebFetch(domain:docs.rs)`, which never covers another host,
and every GitHub path and github.com URL counts as `github.com`. A redirect to another origin is not followed but
reported, so that reading its target is approved afresh, and a host name that
resolves to a local or private address is refused.

`web_search` sends one query to a search service and returns ranked results,
each with its title, URL, date when known and snippet, after a short written
answer when the service gives one. The services are tried in order, and when
one fails the next is asked and the result notes the failure:

| Service    | Set                                             |
| ---------- | ----------------------------------------------- |
| Brave      | `BRAVE_API_KEY` or `BRAVE_SEARCH_API_KEY`       |
| Tavily     | `TAVILY_API_KEY`; also writes an answer         |
| Exa        | `EXA_API_KEY`                                   |
| Perplexity | `PERPLEXITY_API_KEY` or `PPLX_API_KEY`          |
| Kagi       | `KAGI_API_KEY`                                  |
| SearXNG    | `SEARXNG_URL` or `OMC_SEARXNG_URL`, an instance with JSON output on |
| DuckDuckGo | nothing                                         |

Without `OMC_SEARCH`, every service that is set up is tried in that order,
with DuckDuckGo last. `OMC_SEARCH` names the services to try and their order,
as in `OMC_SEARCH=tavily,searxng`; a service it names that is not set up is
skipped and noted, and leaving `duckduckgo` out of it means no search runs
without a key. When no service can be asked, the error lists the variables to
set. DuckDuckGo has no search API, so its no-script results page is read
instead: it needs no account, but its markup can change and it sometimes
answers automated searches with a challenge page, which counts as a failure.
Set a key for searches that must work. Perplexity is asked through its Search
API, which returns results without an answer. `recency` limits results to the
last day, week, month or year where the service can: SearXNG has no week and
widens it to a month, and Kagi has no year and ignores it. Each service gets
20 seconds. A search sends the query off the machine, so it asks for approval
outside yolo mode, under one scope for all searches.
