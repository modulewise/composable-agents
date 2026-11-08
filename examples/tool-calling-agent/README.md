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
message. The answer streams to stdout, and the task's status, finish reason,
and usage go to stderr:

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
