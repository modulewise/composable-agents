# Chat Agent Example

Demonstrates an agent defined in config and invoked with the `agents` CLI. The
`[agent.assistant]` definition becomes a `chat-agent` component importing the
model defined beside it, which becomes the model client for its provider. Each
invocation is one task. The agent sends the message to the model and streams
the reply as the task's artifact.

Each model has its own config:

| Model | Config | Provider | Client |
|---|---|---|---|
| `gemma4:31b` | `gemma.toml` | `ollama` | `chat-completions-client` |
| `gpt-5.4-mini` | `gpt.toml` | `openai` | `chat-completions-client` |
| `claude-haiku-5-5` | `claude.toml` | `anthropic` (`effort = "high"`, `include-reasoning = true`) | `anthropic-messages-client` |

## Prerequisites

- the `agents` CLI: `cargo install --path ../../crates/cli`
- for `ollama`: Ollama serving `gemma4:31b`, or another model specified in
  `ollama.toml`
- for `openai`: an OpenAI API key in `OPENAI_API_KEY`
- for `anthropic`: an Anthropic API key in `ANTHROPIC_API_KEY`
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
./run.sh gemma "Why is the sky blue?"
./run.sh gpt "Why is the sky blue?"
./run.sh claude "Why is the sky blue?"
```

The script lists the agent's metadata, then invokes it with the question
(`gemma` and "Why is the sky blue?" by default). The reply streams to stdout,
and the task's status, finish reason, and usage go to stderr:

```
status: working
The sky looks blue because...
status: completed
finish reason: completed (stop)
usage: ollama gemma4:31b (1 call): input 29, output 341, cache-read 0
```

The message can also be piped to stdin:

```sh
echo "What is 2+2?" | agents invoke gemma.toml -- assistant
```

## Reasoning

`claude.toml` sets `include-reasoning = true`, so each run of the model's
reasoning, when it thinks, shows on stderr as a `reasoning:` line before the
reply. Whether the model thinks is its own choice per request, so a simple
question may show none:

```sh
./run.sh claude "A train leaves at 9:15am. The trip takes 2 hours 15 minutes. Then it waits 30 minutes. When does it leave the station?"
```
