# a2a-server

An [A2A](https://a2a-protocol.org) server for agents that are Wasm components.
It serves a component that exports `composable:agents/agent` with A2A's
JSON-RPC binding (protocol version 1.0), including streaming.

See [examples/a2a](../../examples/a2a) for a server with an agent and a
caller that sends requests to it.

## Run

```sh
cargo install --path crates/a2a-server
a2a-server agent.toml
```

The arguments are definition files (`.toml`) and standalone `.wasm` files.
The server handles `[model]`, `[agent]` and `[a2a-tool]` definitions as the
`agents` CLI does, and `[tool]` and `[toolset]` definitions as
[composable-tools](https://github.com/modulewise/composable-tools) does.

## `[server]`

```toml
[server.assistant]
type = "a2a"
port = 4001
```

| Property | Required | Description |
|---|---|---|
| `type` | yes | `a2a` |
| `port` | yes | the port to listen on |
| `host` | no | the address to listen on (default `127.0.0.1`) |
| `agent-selector` | no | selects the agent to serve, e.g. `name=assistant` (default: every agent) |
| `url` | no | the URL of the server in the agent's card (default `http://<host>:<port>`) |

A server serves exactly one agent, and fails to start when its definition
selects none or more than one. An agent that an interceptor wraps and the
interceptor are two agents, so a server for either needs an `agent-selector`.

With no `[server]` definition of type `a2a`, one server runs on
`127.0.0.1:4001`.

## Agent card

`GET /.well-known/agent-card.json` returns the agent's card. The card has the
agent's metadata, one interface (`JSONRPC`, version `1.0`, at the server's
`url`), and these capabilities:

| Capability | |
|---|---|
| `streaming` | `true` |
| `pushNotifications` | `false` |
| `extendedAgentCard` | `false` |
| `extensions` | the agent's extensions |

The response has an `ETag` and `Cache-Control: max-age=300`, and a request
with that tag in `If-None-Match` gets `304 Not Modified`. The server fails to
start if the agent's metadata has no input mode, no output mode, no skill, or a
skill with no tag, since A2A requires at least one of each.

## Requests

`POST /` takes a JSON-RPC 2.0 request with `Content-Type: application/json`.

- A request names its A2A version in an `A2A-Version` header, or in an
  `A2A-Version` query parameter. Only `1.0` is supported. A request that names
  none has version `0.3`, as A2A specifies, and gets
  `VersionNotSupportedError`.
- An `A2A-Extensions` header lists the extensions the client requests. The
  response's `A2A-Extensions` header lists those the agent activated.
- A message part whose `mediaType` is not one of the agent's input modes (its
  `default-input-modes` and each skill's `input-modes`) gets
  `ContentTypeNotSupportedError`. A media type's parameters and case are
  ignored. A part with no `mediaType` is not checked.
- A response has status `200`, including a JSON-RPC error response.
- A notification (a request with no `id`) is not handled, and gets `204`.
- A request with any other `Content-Type`, or none, gets `415`.
- A body over 4 MiB gets `413`.

| Operation | |
|---|---|
| `SendMessage` | Returns the task once it reaches a terminal or interrupted state, or with `returnImmediately` as soon as the agent has accepted the message. |
| `SendStreamingMessage` | Returns an event stream. Each event is a JSON-RPC response with the task, then each status and artifact update, ending with the final status. |
| `GetTask` | Returns the task. |
| `ListTasks` | Returns a page of tasks, ordered by the time of each task's status, the most recent first. A page has 50 tasks by default and at most 100. |
| `CancelTask` | Cancels the task and returns it once it is canceled. |
| `SubscribeToTask` | Returns an event stream that begins with the task, then has each later update until the task reaches a terminal state. |
| the push notification config operations | `PushNotificationNotSupportedError` |
| `GetExtendedAgentCard` | `UnsupportedOperationError` |

An A2A error has its code and a `google.rpc.ErrorInfo` in its `data`, with the
error's reason and the domain `a2a-protocol.org`:

```json
{
  "code": -32001,
  "message": "no task with id 9f2c",
  "data": [{
    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
    "reason": "TASK_NOT_FOUND",
    "domain": "a2a-protocol.org"
  }]
}
```

## Tasks and contexts

- **A message with no `taskId`** starts a new task. The agent generates the
  task's id, and the context's id if the message has none.
- **A message with the `taskId` of an interrupted task** (`input-required` or
  `auth-required`) continues that task. A message for a task in any other
  state gets `UnsupportedOperationError`, and one for an unknown task gets
  `TaskNotFoundError`.
- **A task's history** has the client's messages, and the message of each
  interrupted or terminal status. The message of a `working` status, such as
  the model's reasoning, is sent in a stream and is not added to the history.
- **A context** keeps the model exchanges of its tasks, which are provided as
  the conversation history of each later task in that context. They are not
  sent to a client.
- **A stream from `SendStreamingMessage`** ends when the task reaches a
  terminal or interrupted state.
- **A stream from `SubscribeToTask`** ends when the task reaches a terminal
  state. It stays open while the task is in an interrupted state, and includes
  the updates of the run that continues the task.
- **Several clients** can subscribe to one task. The server closes the stream
  of a client that reads too slowly, and that client can subscribe again.

Tasks and contexts are (currently) kept in memory, so they are lost when the
server stops. The server keeps 256 tasks, and evicts terminal tasks first, then
interrupted ones, and never a task that is running. It keeps the exchanges of
256 contexts.
