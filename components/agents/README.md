# composable:agents

Agent components that call models through [`composable:models`](../models).

See [`wit/`](wit) for the full definitions.

## Interfaces

- **`types`:** the types of agent requests, events and responses.
- **`agent`:** an agent that handles each message as a task.

The `base-agent` world imports a model `client` and exports `agent`, for an
agent component to include in its own world.

The types mirror [A2A](https://a2a-protocol.org)'s, so an A2A server can host
any agent component. But an agent component is not coupled to A2A, so it can
also be composed and called directly.

## agent

An agent handles each `invoke` as a task:

- **`metadata()`** describes the agent (name, skills, input and output modes),
  which may contribute to an A2A agent card.
- **`invoke(request)`** takes one message, with the conversation's earlier
  model exchanges as `history`, and returns the task's events as they occur
  (status updates and artifact chunks), with a future of the final
  `agent-response`.

The response carries the task's final status, its assembled artifacts, the
model exchanges the invocation made (with each call's token counts, for the
caller to add to the history it passes to later requests), and a finish
reason. An `agent-error` means the request could not be handled as a task,
which is distinct from a task that ran and failed.

## chat-agent

Implements `agent` with one call to a model `client` per message, so it can be
composed with any model client, e.g. `chat-completions-client`. It has no
tools. Text and data parts become text input, and files become image, audio,
video, or document input by media type, which the client may refuse.

The task streams a `working` status, then the model's text as the chunks of one
artifact, with the last chunk marked. Each run of the model's reasoning, when
the client returns it, is sent as one `working` status message whose part has
`{"reasoning": true}` in its metadata. Dropping the event stream cancels the
task and its model call. If the call fails, the task fails with the error as
its status message, and any text already streamed remains as its artifact.

| Key | |
|---|---|
| `name` | required: the agent's name in its metadata |
| `description` | required: the agent's description, also used for its one skill |
| `version` | required: the agent's version in its metadata |
| `instructions` | optional: the model's instructions (`model-settings.instructions`) |

## tool-calling-agent

Implements `agent` with calls to a model `client` in a loop. It imports a
`composable:tools` `toolset`, so it can be composed with any local and/or
remote tools. It shares the tools' metadata with the model, runs tools as
requested by the model, and sends their results back to the model. The task
completes when a model response requests no more tool calls. It fails if a
response's finish reason is a tool request but its output includes no tool
calls. At `max-turns`, it runs the tools the last response requested, then
fails with the finish reason `max-turns`, so the next invocation starts with
their results.

The task streams a `working` status before each model call, and reasoning and
text as in chat-agent, with the text of every call in one artifact. Dropping
the event stream cancels the task at its next event.

A tool's text, images, audio, resource links, and embedded blobs pass to the
model client, which replaces any it can't send with a note. A tool call that
fails returns its error to the model as an error result. If the task ends
before sending a response's tool results, its exchanges end with one that
isn't sent as a call. That includes the results, with an error result for each
tool that didn't run, so every tool call in the history is answered.

| Key | |
|---|---|
| `name` | required: the agent's name in its metadata |
| `description` | required: the agent's description, also used for its one skill |
| `version` | required: the agent's version in its metadata |
| `instructions` | optional: the model's instructions (`model-settings.instructions`) |
| `max-turns` | default: 8; the most model calls allowed per invocation |
| `report-tool-calls` | default: `false`; whether to emit called tool names in status messages |
