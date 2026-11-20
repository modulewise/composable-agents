# Composable Agents

*Work in Progress* 

Agents and model clients as Wasm components that run on
[composable-runtime](https://github.com/modulewise/composable-runtime).

The WIT they implement is in [components/agents](components/agents)
(`composable:agents`) and [components/models](components/models)
(`composable:models`). A client for agents on A2A servers is in
[components/a2a](components/a2a) (`composable:a2a`), and a server for agents
is in [crates/a2a-server](crates/a2a-server).

## Examples

- [examples/chat-agent](examples/chat-agent)
- [examples/tool-calling-agent](examples/tool-calling-agent)
- [examples/a2a](examples/a2a)

## Config

`AgentService` handles `[model]`, `[agent]`, and `[a2a-tool]` definitions. Each
becomes a component with the definition's name.

### `[model]`

```toml
[model.local]
provider = "ollama"
name = "gpt-oss:20b"
```

| Property | Required | Description |
|---|---|---|
| `provider` | yes | `anthropic`, `ollama` or `openai` (more to come) |
| `name` | yes | the provider's model name |
| `max-tokens` | no | the default limit on output tokens per call, overridden by the agent's settings or request |
| `temperature` | no | `ollama` and `openai` only: the default sampling temperature, overridden by the agent's settings |
| `top-p` | no | `ollama` and `openai` only: the default nucleus sampling probability, overridden by the agent's settings |
| `seed` | no | `ollama` and `openai` only: the default sampling seed, overridden by the agent's settings |
| `stop-sequences` | no | a list of strings that each end the model's output, as the default, overridden by the agent's settings |
| `effort` | no | how much effort the model uses on a response. For `anthropic`, one of `low`, `medium`, `high`, `xhigh` or `max`. For `ollama` and `openai`, sent as `reasoning_effort` unchanged, with values that depend on the provider and model |
| `include-reasoning` | no | `anthropic` and `ollama` only: `true` to include the model's reasoning text in the response (default `false`). For `anthropic` it is a summary. `openai` is not supported, since it includes none when called via the Chat Completions API |
| `max-retries` | no | `anthropic` only: how many times a request may be resent after failure (default 2) |

### `[agent]`

```toml
[agent.assistant]
model = "local"
description = "Answers questions concisely"
version = "0.1.0"
instructions = "Answer in one or two sentences."
```

| Property | Required | Description |
|---|---|---|
| `model` | yes | the name of a `[model]` |
| `description` | yes | the agent's description, in its metadata |
| `version` | yes | the agent's version, in its metadata |
| `instructions` | no | the model's system instructions |
| `report-reasoning` | no | `true` to emit the model's reasoning in status messages (default `false`). It has an effect only when the agent's model has `include-reasoning` |
| `toolset` | no | the name of a component exporting `composable:tools/toolset`, such as a `[toolset]` |
| `max-turns` | no | with a `toolset`: the most model calls allowed per invocation (default 8) |
| `report-tool-calls` | no | with a `toolset`: `true` to emit the name of each tool called in a status message (default `false`) |

An agent with a `toolset` is a `tool-calling-agent`, and one without a toolset
is a `chat-agent` (see [components/agents](components/agents)).

### `[a2a-tool]`

```toml
[a2a-tool.weather-agent]
server-url = "http://localhost:9091"
```

| Property | Required | Description |
|---|---|---|
| `server-url` | yes | the URL of the A2A server that hosts the agent |

An A2A tool is an `a2a-tool` component for one agent on an A2A server (see
[components/a2a](components/a2a)). A `[toolset]` lists it by name, like any
other tool.

## CLI

The `agents` CLI lists and invokes the agents in one or more definition files.

```sh
cargo install --path crates/cli
```

`agents list` prints each agent's metadata as JSON, keyed by component name:

```sh
agents list config.toml
```

`agents invoke` sends a message to an agent as a new task. The message is the
argument after the agent's name (or stdin if none is given). The artifact text
streams to stdout, and the task's status, finish reason and usage go to
stderr. It exits successfully only when the task completes.

```sh
agents invoke config.toml -- assistant "Why is the sky blue?"
```

## A2A server

`a2a-server` serves an agent with A2A's JSON-RPC binding. See
[crates/a2a-server](crates/a2a-server).

## License

Copyright (c) 2026 Modulewise Inc and the Composable Agents contributors.

Apache License v2.0: see [LICENSE](./LICENSE) for details.
