# When things go wrong

A request that fails in a way that passes on its own (a rate limit, an
overloaded or failing server, a dropped connection or a stream that ends before
the reply is complete) is sent again, up to 8 times. The wait doubles each
time from about 2 seconds, with some jitter, up to 60 seconds; when the
provider says how long to wait with `retry-after` or `retry-after-ms`, that wait
is kept to, and a provider that asks for more than 60 seconds, however much
more, is not waited for. Each wait shows as a running `retry` step, under an id
no other wait of the process has, saying which attempt it is and why, and
stopping the turn ends it. Sign-in problems, spent quotas, billing, missing
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

The todo list the model keeps with `todo` is watched as well. When it still
has items pending or in progress after 12 calls that changed files since it
last changed, a reminder listing them is added to the results of that step, at
most twice before the list changes again. A call counts when the files it
reports changing really changed, or when it is a successful `write`; a shell
command that only looks, such as `ls` or a test run, does not. When a turn
that wrote the list or changed files is about to end with open items, the
model is sent a notice, once a turn, to finish them or update the list; a list
left over from an earlier turn does not hold up a turn that only answered a
question. Neither happens when the model's reply ends by asking the user a
question.

Nothing in the text of a reply is ever run, so a call the model quotes from a
file or from what the user pasted cannot turn into a real one. When a reply
without tool calls holds what looks like a call written out as text, a block
marked as a call, such as `<tool_call>{"name": "read", "arguments":
{...}}</tool_call>`, or a fenced JSON block that is a call of a known tool or
the arguments of an `edit`, the model is sent a notice, once a turn, that
nothing ran and that it should make a real call if it meant to. Any other code
block, JSON included, is left alone.

Both notices come after an answer that is already kept, in the history and in
the saved conversation. The model's reply to a notice is kept as a reply of its
own, and an empty one ends the turn; it is given to the model joined to the
answer before it, since every provider wants a message from the reader between
two replies. When the request that follows a notice fails, the turn ends with
the answer it already had instead of being undone.
