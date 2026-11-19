# Tool-Calling Agent Example

Demonstrates an agent with tools, defined in config and invoked with the
`agents` CLI. The `[agent.assistant]` definition names a `toolset`, so it
becomes a `tool-calling-agent` component importing its model and its toolset.
Each invocation is one task. The agent calls the model in a loop, running the
tools each response requests, until a response requests none.

The config is split across files, which the CLI combines:

- `agent.toml`: the agent
- `tools.toml`: the toolset, with four calculator tools, each a function of
  the `calculator` component (`calculator.wat`, built by `run.sh`)
- one file per model:

| Config | Provider | Model | Client |
|---|---|---|---|
| `gemma.toml` | `ollama` | `gemma4:31b` | `chat-completions-client` |
| `gpt.toml` | `openai` | `gpt-5.4-mini` | `chat-completions-client` |
| `claude.toml` | `anthropic` | `claude-haiku-5-5` (`effort = "high"`, `include-reasoning = true`) | `anthropic-messages-client` |

## Prerequisites

- the `agents` CLI: `cargo install --path ../../crates/cli`
- `wasm-tools` (`cargo install wasm-tools`)
- for `gemma`: Ollama serving `gemma4:31b`
- for `gpt`: an OpenAI API key in `OPENAI_API_KEY`
- for `claude`: an Anthropic API key in `ANTHROPIC_API_KEY`
- the components pushed to a local registry (until they are published):
  1. Start a registry on `localhost:5001`, e.g. with composable-runtime's
     `examples/local-registry/start-registry.sh`.
  2. Run `../../components/push-to-local-registry.sh`.
  3. `run.sh` sets the environment for the registry. To run `agents`
     directly, set it yourself:

     ```sh
     export COMPOSABLE_OCI_REGISTRY_CONFIG="$(pwd)/../../components/registries.toml"
     export COMPOSABLE_OCI_ALLOW_LATEST=1
     ```

## Run

```sh
./run.sh gemma
./run.sh gpt
./run.sh claude
```

The script lists the agent's metadata, then invokes it with the question
(`gemma` and "What is 17 times 23, minus 5?" by default). `agent.toml` sets
`report-tool-calls = true`, so each tool call shows on stderr as a status
message, and `report-reasoning = true`, so any reasoning the model returns
shows as a `reasoning:` line. The answer streams to stdout, and the task's
status, finish reason, and usage go to stderr:

```
status: working
status: working: Calling tool `multiply`
status: working
status: working: Calling tool `subtract`
status: working
17 times 23 minus 5 is 386.
status: completed
finish reason: completed (stop)
usage: ollama gemma4:31b (3 calls): input 840, output 146, cache-read 834
```

The usage line sums the task's model calls for each provider and model.

To invoke the agent directly (set the environment variables shown above first):

```sh
agents invoke agent.toml tools.toml gpt.toml -- assistant "What is 144 divided by 12, plus 3?"
```

## Tracing

```sh
./run.sh --otel gemma
```

With `--otel`, the script loads `traced-agent.toml` in place of `agent.toml`
and invokes `traced-assistant`. That file defines the same `assistant`, except
that it imports `traced-model`, and two interceptors that emit spans following
the OpenTelemetry GenAI semantic conventions:

- `traced-assistant` is `agent-otel-interceptor`, which wraps `assistant` and
  emits an `invoke_agent assistant` span for each invocation, with the agent's
  name, description and version, the conversation id, the finish reason, and
  the token counts summed over the invocation's model calls.
- `traced-model` is `model-client-otel-interceptor`, which wraps `model` and
  emits a `chat` span for each model call, as a child of the invocation's span,
  with the model, provider and server, the parameters sent, the finish reason,
  the token counts, and the time to the first chunk.

The spans go to an OTLP collector on `localhost:4317`, such as Jaeger, under
the service name `tool-calling-agent-example`. The `otel` capability exports
them, and both interceptors also import the `wasi:clocks` and `wasi:random`
capabilities. See [components/agents](../../components/agents) and
[components/models](../../components/models) for the attributes each records.
