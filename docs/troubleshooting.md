# When things go wrong

A request that fails in a way that passes on its own (a rate limit, an
overloaded or failing server, a dropped connection or a stream that ends before
the reply is complete) is sent again, up to 8 times. The wait doubles each
time from about 2 seconds, with some jitter, up to 60 seconds; when the
provider says how long to wait with `retry-after` or `retry-after-ms`, that wait
is kept to, and a provider that asks for more than 60 seconds, however much
more, is not waited for. Each wait shows as a running `retry` step, under an id
no other wait of the process has, saying which attempt it is and why, and
stopping the turn ends it. When the attempts are used up, or the provider
asks for a longer wait than is waited for, the turn fails with the reason, but
its prompt and every step it finished stay in the conversation, unanswered,
and stay saved; the turn is not recorded as lost. The reason the client is
told ends by saying so: `/retry` sends the prompt again, a new prompt is sent
after it so the model sees both, and a conversation reopened before either can
go on with it through `Resume`. Sign-in problems, spent quotas, billing, missing
credits and requests the provider will not take fail the turn at once, and so
does a reply the model declined to give or the provider's content filter
stopped.

A reply that is empty, or that the output limit or a full context window cut
off before any tool call,
is not kept: the model is sent a short notice, which the client never shows as
a turn and the history does not keep, and asked again, up to three times a
turn. A reply cut off part of the way through its text is carried on and kept
as one reply. The tool calls of a reply the output limit cut off are never run:
each fails with the reason, and the model is told to take a smaller step, up
to three times a turn, counted apart from the other recoveries, before the
turn fails. Arguments sent as a JSON string holding an object are read out of
it; a call whose arguments are not a JSON object fails with what is wrong with
them. Arguments are put right against the tool's schema before the call is
shown or approved, so what the reader approves is exactly what runs.

When the model makes the same calls with the same arguments five times in a
row, or makes three edits in a row to one file that leave it as it was, a
notice telling it to change course is added to the results of that step, and
after six such edits it is told to stop and explain to the user.

When a reply writes text beside its tool calls, a notice telling the model to
make its calls without text and keep its words for the final answer is added
to the results of that step, at most twice a turn. That text reaches the
client as thinking, not as the answer.

After a turn that changed files, a final answer of more than two lines with
text is not shown: the model is asked once, with a notice, to write it again
as one paragraph of at most two lines, and that answer is shown and kept.

The todo list the model keeps with `todo` is watched as well. When it still
has items pending or in progress after 12 calls that changed files since it
last changed, a reminder listing them is added to the results of that step, at
most twice before the list changes again. A call counts when the files it
reports changing really changed, or when it is a successful `write`; a shell
command that only looks, such as `ls` or a test run, does not. When a turn
that wrote the list or changed files is about to end with open items, the
model is sent a notice to finish them or update the list, up to three times a
turn; a reply to a notice that makes no tool call ends the turn. A list left
over from an earlier turn does not hold up a turn that only answered a
question. Neither happens when the model's reply ends by asking the user a
question.

These notices come after an answer that is already kept, in the history and in
the saved conversation. The model's reply to a notice is kept as a reply of its
own, and an empty one ends the turn; it is given to the model joined to the
answer before it, since every provider wants a message from the reader between
two replies. When the request that follows a notice fails, the turn ends with
the answer it already had instead of being undone.

## Crashes and bug reports

Every panic of a crab process is recorded in `crashes.json` in crab's data
folder (`~/.local/share/oh-my-crab` on Linux), which keeps the last five, with
the message, where it happened and the call stack. The next `crab serve` or
`crab exec` says on standard error that crab crashed, once per crash and only
for a crash of the last seven days.

`crab bug-report [--note <text>] [folder]` writes
`crab-bug-report-<time>.zip` into `folder`, or the current folder, and prints
its path. It holds `report.json`, with crab's version, the operating system,
the architecture and the note, and `crashes.json`, the crash log, when there
is one. Nothing else is read, and nothing is sent anywhere: attach the zip to
the report yourself.

## Telemetry

A crab built with the `otlp` feature (`cargo install --path crates/omc
--features otlp`) sends traces and metrics over OTLP, HTTP with protobuf, while
`serve` or `exec` runs, when `OTEL_EXPORTER_OTLP_ENDPOINT`, or its `_TRACES_`
or `_METRICS_` form, is set, and `OTEL_SDK_DISABLED` is not `true`. The other
standard `OTEL_*` variables, such as `OTEL_EXPORTER_OTLP_HEADERS`, apply.
Without an endpoint, nothing is started. A build without the feature says on
standard error that it cannot send.

Each turn is an `invoke_agent` span, with a `chat <model>` span for every
model request, carrying the provider, the model and the tokens, and an
`execute_tool <name>` span for every tool call. The metrics are
`gen_ai.client.token.usage`, by token type, provider and model, and
`gen_ai.client.operation.duration`, in seconds.
