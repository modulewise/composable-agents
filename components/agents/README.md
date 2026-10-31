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

## chat-completions-client

Implements `model-client` for the Chat Completions API, served by OpenAI,
Ollama and other providers. It takes text and images (as `image_url` parts),
and fails on audio, video, and documents. It maps `output-schema` to a strict
JSON Schema `response_format`. Its config:

| Key | |
|---|---|
| `model` | required |
| `provider` | required: the provider name each call reports |
| `base-url` | default: `https://api.openai.com/v1` (set one for different providers) |
| `api-key` | optional: sent as a bearer token when set (most hosted providers require one) |
| `max-tokens` | optional default, overridden by `model-settings` and the request |

Example: [`examples/chat-completions`](../../examples/chat-completions).

> [!NOTE]
>
> When configuring a model that requires it, the api-key is passed within the
> generated config component. That will change once secret management support
> is available (on the roadmap). This project is still in an early development
> phase, not yet intended for any "real world" usage.
