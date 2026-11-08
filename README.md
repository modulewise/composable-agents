# Composable Agents

*Work in Progress* 

Agents and model clients as Wasm components that run on
[composable-runtime](https://github.com/modulewise/composable-runtime).

The WIT they implement is in [components/agents](components/agents)
(`composable:agents`) and [components/models](components/models)
(`composable:models`).

## Examples

- [examples/chat-agent](examples/chat-agent)
- [examples/tool-calling-agent](examples/tool-calling-agent)

## Config

`AgentService` handles `[model]` and `[agent]` definitions. Each becomes a
component with the definition's name.

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
| `effort` | no | `anthropic` only: `low`, `medium`, `high`, `xhigh` or `max`, how much effort the model uses on a response |
| `include-reasoning` | no | `anthropic` only: `true` to include a summary of the model's reasoning in the response (default `false`) |
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
| `toolset` | no | the name of a component exporting `composable:tools/toolset`, such as a `[toolset]` |
| `max-turns` | no | with a `toolset`: the most model calls allowed per invocation (default 8) |
| `report-tool-calls` | no | with a `toolset`: `true` to emit the name of each tool called in a status message (default `false`) |

An agent with a `toolset` is a `tool-calling-agent`, and one without a toolset
is a `chat-agent` (see [components/agents](components/agents)).

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

## License

Copyright (c) 2026 Modulewise Inc and the Composable Agents contributors.

Apache License v2.0: see [LICENSE](./LICENSE) for details.
