# Chat Agent Example

Demonstrates an agent defined in config and invoked with the `agents` CLI. The
`[agent.assistant]` definition in `config.toml` becomes a `chat-agent`
component importing the model defined by `[model.local]`, which becomes a
`chat-completions-client` component calling Ollama. Each invocation is one
task. The agent sends the message to the model, and streams the reply as the
task's artifact.

## Prerequisites

- the `agents` CLI: `cargo install --path ../../crates/cli`
- Ollama serving `gpt-oss:20b`, or another model specified in `config.toml`
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
./run.sh "Why is the sky blue?"
```

The script lists the agent's metadata, then invokes it with the question. The
reply streams to stdout, and the task's status, finish reason and usage go to
stderr:

```
status: working
The sky looks blue because...
status: completed
finish reason: completed (stop)
usage: input 88, output 84, total 172, cache-read 0
```

The message can also be piped to stdin:

```sh
echo "What is 2+2?" | agents invoke config.toml -- assistant
```
