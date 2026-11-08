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
  the conversation's earlier exchanges, and fails on history it cannot use.
- **`call(request)`** makes one model call and returns the response as it
  arrives, with a stream of deltas (content fragments and token counts) and a
  future of the final `model-response`. A request's `max-tokens` limits the
  call, taking precedence over the session's.
- **`exchanges()`** returns the calls made within the session (each request
  with its output, provider, models, and token counts), for the caller to add
  to the conversation's history.

A response that never finishes (a stream that ends without a finish reason,
or carries a provider error) fails the call with the provider's message.

Token counts are recorded in each call's exchange as the provider reports
them, so a call that fails or is canceled keeps the counts reported before it
ended. A `usage` delta carries the counts so far, and the final
`model-response` or `model-error` carries the call's counts. Any count that is
`none` means it was not reported.

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
| `effort` | optional: `low`, `medium`, `high`, `xhigh` or `max`, sent as `output_config.effort` |
| `include-reasoning` | default: `false`; `true` asks for adaptive thinking with a summary in each response |
| `max-retries` | default: 2; how many times a failed request may be resent |

It imports `wasi:clocks/monotonic-clock` for any waits between retries.

> [!NOTE]
>
> When configuring a model that requires it, the api-key is passed within the
> generated config component. That will change once secret management support
> is available (on the roadmap). This project is still in an early development
> phase, not yet intended for any "real world" usage.

Example: [`examples/model-clients`](../../examples/model-clients)
