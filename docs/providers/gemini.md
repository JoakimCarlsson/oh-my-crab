# Google Gemini

The `gemini` provider speaks Google's own Gemini API (`generateContent`), not
its OpenAI-compatible endpoint. Replies stream from
`models/<model>:streamGenerateContent?alt=sse`.

## Signing in

Set `GEMINI_API_KEY`, or `GOOGLE_API_KEY` when the first is unset, to a key from
Google AI Studio; a key a client saved works too. The key goes in the
`x-goog-api-key` header.

The address is `https://generativelanguage.googleapis.com/v1beta`, or
`OMC_GEMINI_URL`. An address ending in `/openai`, as one written for the
OpenAI-compatible endpoint does, has that ending dropped, so an older override
keeps working.

## Models

The default model is `gemini-3.8-flash`. The model list is the API's
`models` list, all pages of it, keeping the models that take
`generateContent` and dropping embedding, image, video, speech and live
models; the default leads it.

Context windows, output limits, input kinds and prices come from the model
catalog's `google` provider. A model the catalog does not describe is given a
window of 1,048,576 tokens and no price.

## What is sent

- The system prompt goes as `systemInstruction`.
- Prompts are `user` turns, replies `model` turns; turns of the same role in a
  row are joined, since Gemini wants them to alternate.
- Images go as `inlineData` parts.
- Tools go as one `functionDeclarations` entry, with
  `functionCallingConfig` set to `AUTO`. Each tool's JSON Schema is converted
  to the schema Gemini takes for `parameters`: `$ref`s are inlined, `allOf` is
  merged, `oneOf` becomes `anyOf`, a `null` type or branch becomes `nullable`,
  a list of types becomes one type or an `anyOf`, `const` becomes a one-value
  `enum`, enum values become strings, and keywords Gemini does not know, such
  as `additionalProperties`, `$schema` and unsupported `format`s, are dropped.
- Tool results go back as `functionResponse` parts, as `output` or, for a
  failed call, `error`. On Gemini 3 and later the images a tool returned go
  inside the response; on older models they follow in the same `user` turn,
  after a line naming the call.

## Thinking

Gemini 2.5 and later think, and are asked for thought summaries
(`includeThoughts`), which stream as thinking. The effort sets
`thinkingConfig`:

| Effort         | Gemini 3 and later (`thinkingLevel`) | Gemini 2.5 (`thinkingBudget`) |
| -------------- | ------------------------------------ | ----------------------------- |
| `low`          | `low`                                | 1,024                         |
| `medium`       | `medium`                             | 8,192                         |
| `high`         | `high`                               | 16,384                        |
| `xhigh`        | not offered                          | 24,576                        |
| none picked    | the model's own default              | the model's own default       |

Earlier models are sent no `thinkingConfig`.

Gemini signs its reasoning with `thoughtSignature`s on the parts of a reply,
most often on its function calls. crab keeps each signature in the reply's
opaque blocks (`{"type": "gemini_thought_signature", "part": ..., "signature":
...}`, where `part` is `thinking`, `text` or a call's id) and sends it back on
the same part with the next request, as Gemini requires for its reasoning to
carry over between tool calls. A function call in the history that Gemini did
not sign, such as one made by another provider earlier in the conversation, is
sent with the signature Gemini documents for skipping the check.

Function call ids Gemini gave are sent back with the call and its response;
calls it sent without one get an id of crab's own, which is not sent.

## Usage and cost

`usageMetadata` gives the tokens: cached input (`cachedContentTokenCount`) is
counted apart from the rest of the prompt, and thinking tokens count as output.
Costs use the catalog's prices.

## Ending

`STOP` ends a reply and `MAX_TOKENS` marks it cut at the output limit. Any
other finish reason, such as `SAFETY`, `RECITATION` or
`MALFORMED_FUNCTION_CALL`, a blocked prompt and an error inside the stream fail
the request; a stream that ends without a finish reason is unfinished and can
be sent again.

## The OpenAI-compatible endpoint

The built-in `gemini` provider no longer uses Google's OpenAI-compatible
endpoint. To reach it, define a Chat Completions provider in `providers.json`
with the address `https://generativelanguage.googleapis.com/v1beta/openai`, as
[custom endpoints](custom-endpoints.md) says.
