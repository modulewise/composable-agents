# A2A Example

Serves an agent on an [A2A server](../../crates/a2a-server), then calls it
with A2A's JSON-RPC requests and from another agent through an
[a2a-tool](../../components/a2a).

The server serves the agent of another example:

| Agent | Example |
|---|---|
| `chat-agent` | [../chat-agent](../chat-agent) |
| `tool-calling-agent` | [../tool-calling-agent](../tool-calling-agent) |

## Prerequisites

- the prerequisites of the example whose agent is served
- the `a2a-server` binary: `cargo install --path ../../crates/a2a-server`
- the `agents` CLI: `cargo install --path ../../crates/cli`
- `curl` and `jq`

## Serve

```sh
./run.sh serve chat-agent
./run.sh serve tool-calling-agent claude
```

The model is `gemma` unless one is given. The server loads the example's own
files with `server.toml`, which defines the A2A server:

```toml
[server.assistant]
type = "a2a"
port = 4001
```

It serves until it is stopped.

## Call with JSON-RPC

In another terminal:

```sh
./run.sh card
./run.sh send "What is 17 times 23, minus 5?"
./run.sh stream "What is 17 times 23, minus 5?"
```

- `card` prints the agent's card, from `/.well-known/agent-card.json`.
- `send` sends a `SendMessage` request and prints the response, which has the
  task with its final status and its artifacts.
- `stream` sends a `SendStreamingMessage` request and prints the event stream.
  It begins with the task, then has each status and artifact update, and ends
  with the task's final status.

Each prints JSON, so it can be piped to `jq`:

```sh
./run.sh send "What is 17 times 23, minus 5?" | jq '.result.task.artifacts[].parts[].text'
```

## Call from an agent

`caller.toml` defines an agent whose only tool is the agent on the server:

```toml
[a2a-tool.remote-agent]
server-url = "${process:env:A2A_SERVER_URL|http://127.0.0.1:4001}"
```

```sh
./run.sh call "What is 17 times 23, minus 5?"
```

The `caller` agent sends the message to the remote agent with its tool, and
replies with what the remote agent returns:

```
status: working
status: working: Calling tool `remote-agent`
status: working
17 times 23, minus 5 is 386.
status: completed
```

`MODEL` selects the caller's model, e.g. `MODEL=claude ./run.sh call "..."`.

## Call another A2A server

`card`, `send` and `stream` use `A2A_SERVER_URL` when it is set, and `call`
takes the server's URL as its second argument. For example, with the
[helloworld sample](https://github.com/a2aproject/a2a-samples/tree/main/samples/python/agents/helloworld)
on port 9999:

```sh
A2A_SERVER_URL=http://127.0.0.1:9999 ./run.sh card
./run.sh call "Say hello" http://127.0.0.1:9999
```

## Tracing

```sh
./run.sh serve --otel tool-calling-agent
```

With `--otel`, the server serves the traced agent of the tool-calling-agent
example, which exports a span for each invocation and each model call to an
OTLP collector on `localhost:4317`. That example's `traced-agent.toml` defines
two components that export the agent interface, `traced-assistant` and the
`assistant` it wraps. A server serves exactly one agent, so
`traced-server.toml` selects it, and exports the server's spans to the same
collector:

```toml
[server.assistant]
type = "a2a"
port = 4001
agent-selector = "name=traced-assistant"
otlp-endpoint = "http://localhost:4317"
```

Each request then has a trace with the server's span for the A2A method, the
agent's `invoke_agent` span as its child, and the model's `chat` spans beneath
that.
