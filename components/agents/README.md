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
- **`invoke(request, cancel)`** takes one message, with the conversation's
  earlier model exchanges as `history`, and returns an `agent-invocation` with
  the task's ids, the extensions the agent activated, and the response. The
  response is the task's events as they occur (status updates and artifact
  chunks), with a future of the final `agent-response`. The task is canceled
  when `cancel` resolves, or when the caller drops its writer. A caller that
  stops reading the events does not cancel the task.

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
`{"reasoning": true}` in its metadata. A cancel stops the model call, and the
task ends `canceled` with the call's exchange. If the call fails, the task
fails with the error as its status message, and any text already streamed
remains as its artifact.

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
text as in chat-agent, with the text of every call in one artifact. A cancel
stops the model call or tool call that is running, and the task ends
`canceled`, with its exchanges and a closing exchange that answers the tool
calls that didn't run.

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

## Interceptors

An interceptor exports `agent` and also imports it, wrapping another agent.
`agent-interceptor-support` provides `invoke` and `metadata` functions that
relay each task's response, so an interceptor implements one constructor and
any callbacks it needs:

- `AgentInterceptor`: `new_invocation` (creates an invocation's interceptor,
  and may change the request, or return an error, in which case no task
  starts), plus the `on_metadata` callback.
- `InvocationInterceptor`: callbacks include `on_invocation` (sees the
  `agent-invocation`), `on_event` (sees each event, and returns an `Action`),
  `on_result` (sees the outcome the caller receives) and `on_error` (sees the
  error the agent returned instead of starting a task, and returns the error
  the caller receives).

Each callback has a no-op default. The callbacks after the agent gets the
request see its events and outcome but can't change them, since the agent
assembles its response's artifacts and exchanges from its own copies.
`on_event` returning `Action::Reject(message)` rejects the task. The event
isn't relayed, the caller's events end, the wrapped agent's task is canceled,
and the outcome is the agent's response with the `rejected` state and that
status message. `agent_message` builds that message from the event's task and
context ids, with a message id provided by the interceptor.

The wrapped agent gets its own cancel future, which `invoke` resolves when the
caller's resolves or the task is rejected. Once the caller stops reading the
events, `on_event` still sees each one, which is then dropped. An
`AgentInterceptor` also creates each task's stream and futures from its own
bindings, as an agent does.

## agent-otel-interceptor

Emits an `invoke_agent {name}` internal span for each invocation, as a child of
the current span, following the OpenTelemetry GenAI semantic conventions. The
span is open while the agent runs, so the `chat` spans of its model calls are
its children. It records the agent's name, description, and version (from its
`metadata`), the conversation id (the invocation's `context-id`), the output
type when the request accepts exactly one output mode that has a type, the
finish reason, and the input and output tokens summed over the invocation's
model calls. A task that ends `failed` or `rejected`, or an error from the
agent, has an `Error` status and an `error.type` (`_OTHER`, `rejected`, or the
error's code), and a `gen_ai.invoke_agent.exception` log record.

It imports the system clock, random, and patched `wasi:otel` (`0.2.0-rc.2+p3`)
WIT for tracing, logs, and types. Those are provided by the composable-runtime
`wasi:clocks`, `wasi:random`, and `otel` capabilities.
