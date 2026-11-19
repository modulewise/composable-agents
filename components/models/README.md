# composable:models

Model client components, using a common abstraction over language model APIs.

See [`wit/`](wit) for the full definitions.

## Interfaces

- **`types`:** the types of model calls.
- **`client`:** a session that makes model calls for one agent invocation.

Agents ([`composable:agents`](../agents)) call a model through `client`.

## client

A `session` serves one agent invocation:

- **`constructor(settings, history)`** takes the caller's `model-settings` and
  the conversation's earlier exchanges. It fails with a `session-error` that
  names the input the client couldn't use, `history` or `settings` from the
  caller, or `config` of its own, such as a `base-url` that is not a valid URL
  or has no host or port.
- **`call(request)`** makes one model call and returns a `model-call` with the
  requested model, the provider, the API, the server it's sent to, the
  parameters sent, and the response as it arrives. The response is a stream of
  deltas (content fragments and token counts) and a future of the final
  `model-response`. A request's `max-tokens` limits the call, taking precedence
  over the session's, then the config's, then the client's default. `call`
  fails, with nothing sent or recorded, when no client receives the call (e.g.
  an interceptor refuses it). The response is an error, also with nothing sent
  or recorded, on input the client cannot send, or when the session's previous
  call's future has not resolved, since a session's calls are sequential. A
  failure sending the request or reading the response resolves the future.
- **`exchanges()`** returns the calls made within the session (each request
  with its output, provider, models, and token counts), for the caller to add
  to the conversation's history. A call's exchange is complete once the call's
  future resolves.

A response that never finishes (a stream that ends without a finish reason,
or carries a provider error) fails the call with the provider's message.

Token counts are recorded in each call's exchange as the provider reports
them, so a call that fails or is canceled keeps the counts reported before it
ended. A `usage` delta includes the counts so far, and the final
`model-response` includes the call's counts. Any count that is `none` means it
was not reported.

History follows four rules:

- **Every tool call is answered** by a tool result in the next exchange's
  request. The APIs reject a conversation with an unanswered call.
- **The session replays reasoning** from earlier outputs, with any opaque
  state unchanged.
- **Dropping the deltas cancels the call.** The client stops reading and
  closes the connection, so the provider will stop generating.
- **Nothing is dropped silently.** A new call fails on input the client
  cannot send. In history, such input and any media the API cannot carry are
  replaced by a note so that a conversation can continue on any client.

Tool names have any `.` replaced with `_` when passed between the client and
the model, since not every API supports `.`. Any tool names that would collide
(e.g. `a.b` and `a_b`) will fail. Any tool name longer than 128 characters will
also fail.

## chat-completions-client

Implements `client` for the Chat Completions API, served by OpenAI, Ollama and
other providers. It takes text and images (as `image_url` parts), and fails on
audio, video, and documents. It maps `output-schema` to a strict JSON Schema
`response_format`.

| Key | |
|---|---|
| `model` | required |
| `provider` | required: the provider name each call reports |
| `base-url` | default: `https://api.openai.com/v1` (set one for different providers) |
| `api-key` | optional: sent as a bearer token when set (most hosted providers require one) |
| `max-tokens` | optional default, overridden by `model-settings` and the request |
| `max-tokens-field` | default: `max_completion_tokens`; `max_tokens` for providers that still use it (such as Ollama) |
| `temperature` | optional default, overridden by `model-settings` |
| `top-p` | optional default, overridden by `model-settings` |
| `seed` | optional default, overridden by `model-settings` |
| `stop-sequences` | optional default, a JSON array of strings (e.g. `'["END"]'`), overridden by `model-settings` |
| `effort` | optional: sent as `reasoning_effort` unchanged, since the values depend on the provider and model |
| `include-reasoning` | default: `false`; `true` includes the reasoning text a provider sends in each response |

It fails a session when `top-k` is configured, since the API has no `top_k`. A
`top-k` in `model-settings` isn't sent.

With `include-reasoning`, the text in a chunk's `reasoning` field is streamed
as reasoning deltas and is the first item of the response's output. Without it,
that text is discarded. OpenAI includes none when called via the Chat
Completions API. Ollama sends it for a model that thinks. Reasoning is not sent
back with later calls, since the API has no form for it.

Whether a setting has an effect depends on the provider and model. With Ollama,
some models ignore stop sequences, and OpenAI rejects them for some models with
a 400.

## anthropic-messages-client

Implements `client` for Anthropic's Messages API (version `2026-10-21`). It
accepts text, images, and PDF or text documents, but fails on audio, video, or
other documents. It maps `output-schema` to a JSON Schema
`output_config.format`.

Each response is kept in `opaque-content` as sent by the API, and a later
session replays it exactly, so the model's thinking remains valid throughout
the conversation.

A request is retried on a 5xx, on a 429 with a `retry-after` header, or when it
fails to send. It waits `retry-after` seconds if provided, otherwise it backs
off exponentially (0.5s doubling up to 8s).

| Key | |
|---|---|
| `model` | required |
| `provider` | required: the provider name each call reports |
| `base-url` | default: `https://api.anthropic.com/v1` |
| `api-key` | optional: sent as a bearer token when set |
| `max-tokens` | default: 64000, overridden by `model-settings` and the request |
| `stop-sequences` | optional default, a JSON array of strings (e.g. `'["END"]'`), overridden by `model-settings` |
| `effort` | optional: `low`, `medium`, `high`, `xhigh` or `max`, sent as `output_config.effort` |
| `include-reasoning` | default: `false`; `true` asks for adaptive thinking with a summary in each response |
| `max-retries` | default: 2; how many times a failed request may be resent |

It fails a session when `temperature`, `top-p`, `top-k` or `seed` is
configured, since the API rejects the first three and has no `seed`. Those in
`model-settings` aren't sent.

It imports `wasi:clocks/monotonic-clock` for any waits between retries.

> [!NOTE]
>
> When configuring a model that requires it, the api-key is passed within the
> generated config component. That will change once secret management support
> is available (on the roadmap). This project is still in an early development
> phase, not yet intended for any "real world" usage.

Example: [`examples/model-clients`](../../examples/model-clients)

## Interceptors

An interceptor exports `client` and imports it, wrapping another client.
`model-client-interceptor-support` provides a `Session` that relays each
response, so an interceptor implements two constructors and any callbacks it
needs:

- `SessionInterceptor`: `new` (creates the session's interceptor, sees the
  history, and may change the settings, or fail the session) and `new_call`
  (creates a call's interceptor, and may change the request, or refuse the
  call).
- `CallInterceptor`: callbacks include `on_call` (sees the `model-call`),
  `on_delta` (sees each delta, and returns an `Action`), `on_result` (sees the
  outcome the caller receives, and whether the caller canceled the call), and
  `on_error` (sees the error the call returned instead of a response, and
  returns the error the caller receives).

Each callback has a default that changes nothing. The callbacks after the
client gets the call see its output but can't change it, since the client
keeps that output in its own state for the session's next call. `on_delta`
returning `Action::Reject(reason)` blocks the call's output, as a provider's
content filter does. The delta isn't relayed, the caller's deltas end, the
client's call is canceled, and the outcome is a response with no output, the
`blocked(reason)` finish reason (e.g. `content_filter`), and the counts from
the last usage delta. A refusal in `new_call` is an error instead, and nothing
is sent. A `SessionInterceptor` also creates each call's stream and future
from its own bindings, as a client does.

## model-client-otel-interceptor

Emits a `chat {model}` client span for each call, as a child of the host's
span, following the OpenTelemetry GenAI semantic conventions. It records the
requested model, provider, server and parameters sent, the response's model,
id, finish reason and token counts, and the time to the first chunk. A failed
call's span has an `Error` status and an `error.type` (the provider's error
code, else the status code, else `_OTHER`), and a
`gen_ai.client.inference.exception` log record. A call the caller canceled
isn't a failure, so its status stays unset, with the finish reason `error`. A
`blocked` response, from the provider or an interceptor's `Reject`, is a
response, so its status also stays unset, with its finish reason. A call
dropped before it ends is recorded as canceled.

A call that no client received (e.g. an interceptor below refused it, or a
router found no route) fails before a provider or model is chosen. Its span is
named `chat`, and has no `gen_ai.provider.name` or `gen_ai.request.model`,
although the conventions describe the provider as being required.

It imports a patched version of `wasi:otel` tracing, logs, and types
(`0.2.0-rc.2+p3`), the system clock, and random, which composable-runtime's
`otel`, `wasi:clocks` and `wasi:random` capabilities provide.
