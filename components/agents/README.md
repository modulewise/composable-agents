# composable:agents

WIT for agents and the model-clients they call.
See [`wit/`](wit) for the full definitions.

## Interfaces

- **`types`:** the types used by both interfaces.
- **`model-client`:** a common abstraction over language model APIs.
- **`agent`:** an agent that handles each message as a task.

The types mirror [A2A](https://a2a-protocol.org)'s, so an A2A server can host
any agent component. But an agent component is not coupled to A2A, so it can
also be composed and called directly.

## model-client

A `session` serves one agent invocation:

- **`constructor(settings, history)`** takes the caller's `model-settings` and
  the conversation's earlier exchanges, and fails on history it cannot use.
- **`call(request)`** makes one model call and returns the response as it
  arrives, with a stream of deltas and a future of the final `model-response`.
  A request's `max-tokens` limits the call, taking precedence over the
  session's.
- **`exchanges()`** returns the calls made within the session (each request
  with its output), for the caller to maintain and pass to a later session.

A response that never finishes (a stream that ends without a finish reason,
or carries a provider error) fails the call with the provider's message.
Any token usage count is `none` when not reported, and `unreported-calls`
is the total count of calls that reported none, indicating that the sum may
represent a subset of actual token usage.

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

Implements `model-client` for the Chat Completions API, served by OpenAI,
Ollama and other providers. It takes text and images (as `image_url` parts),
and fails on audio, video, and documents. It maps `output-schema` to a strict
JSON Schema `response_format`.

| Key | |
|---|---|
| `model` | required |
| `provider` | required: the provider name each call reports |
| `base-url` | default: `https://api.openai.com/v1` (set one for different providers) |
| `api-key` | optional: sent as a bearer token when set (most hosted providers require one) |
| `max-tokens` | optional default, overridden by `model-settings` and the request |
| `max-tokens-field` | default: `max_completion_tokens`; `max_tokens` for servers that read only that one (such as Ollama) |

## anthropic-messages-client

Implements `model-client` for Anthropic's Messages API (version `2026-10-21`).
It accepts text, images, and PDF or text documents, but fails on audio, video,
or other documents. It maps `output-schema` to a JSON Schema
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
| `include-reasoning` | default: `false`; `true` asks for adaptive thinking with a summary of it in each response |
| `max-retries` | default: 2; how many times a failed request is sent again (0 for none) |

It imports `wasi:clocks/monotonic-clock` for any waits between retries.

> [!NOTE]
>
> When configuring a model that requires it, the api-key is passed within the
> generated config component. That will change once secret management support
> is available (on the roadmap). This project is still in an early development
> phase, not yet intended for any "real world" usage.

Example: [`examples/model-clients`](../../examples/model-clients)

## agent

An agent handles each `invoke` as a task:

- **`metadata()`** describes the agent (name, skills, input and output modes),
  which may contribute to an A2A agent card.
- **`invoke(request)`** takes one message, with the conversation's earlier
  model exchanges as `history`, and returns the task's events as they occur
  (status updates and artifact chunks), with a future of the final
  `agent-response`.

The response carries the task's final status, its assembled artifacts, the
model exchanges the invocation added (for the caller to keep and pass as the
next request's `history`), the token usage, and a finish reason. An
`agent-error` means the request could not be handled as a task, which is
distinct from a task that ran and failed. An agent that does not enforce a
request's `token-budget` refuses the request.

## chat-agent

Implements `agent` with one call to a `model-client` per message, so it can be
composed with any client, e.g. `chat-completions-client`. It has no tools. Text
and data parts become text input, and files become image, audio, video, or
document input by media type, which the client may refuse. It does not enforce
token budgets yet, so it refuses any request that includes one.

The task streams a `working` status, then the model's text as the chunks of one
artifact, with the last chunk marked. Each run of the model's reasoning, when
the client returns it, is sent as one `working` status message whose part has
`{"reasoning": true}` in its metadata, and is not part of the artifact.
Dropping the event stream cancels the task and its model call. If the call fails, the task fails
with the error as its status message, and any text already streamed remains its
artifact. Its config:

| Key | |
|---|---|
| `name` | required: the agent's name in its metadata |
| `description` | required: the agent's description, also used for its one skill |
| `version` | required: the agent's version in its metadata |
| `instructions` | optional: the model's instructions (`model-settings.instructions`) |
