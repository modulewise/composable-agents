# Model Clients Example

Demonstrates calling a model directly through a model client. The `model`
component is implemented here as an example entry point (as opposed to using
an agent component that plays that same role). It imports a model client
component, which exports the `composable:agents/model-client` interface as
defined in [WIT](../../components/agents/wit). Each time the model component's
exported `chat` function is invoked, it opens a session, sends the prompt
within a request, and returns the reply as text. When an output schema is
passed, the reply JSON will match it.

Each client has its own config:

| Client | Config | Calls |
|---|---|---|
| `chat-completions-client` | `chat-completions.toml` | the Chat Completions API, here Ollama serving `gpt-oss:20b` |
| `anthropic-messages-client` | `anthropic-messages.toml` | Anthropic's Messages API, here `claude-haiku-5-5` |

## Prerequisites

- `composable` (composable-runtime's CLI)
- `wasm-tools`
- `jq`
- a rust toolchain with the `wasm32-unknown-unknown` target
- the model clients built: `../../components/build.sh`
- for `chat-completions`: Ollama serving `gpt-oss:20b`, or another model
  specified in `chat-completions.toml` (see below)
- for `anthropic-messages`: an Anthropic API key in `ANTHROPIC_API_KEY`

## Build

```sh
./build.sh
```

## Run

```sh
./run.sh chat-completions Japan
./run.sh anthropic-messages Japan
```

The script asks the model, through the given client (`chat-completions` by
default), for the capital of the given place (France by default) and its
population, passing an output schema for the reply.

Output will match the schema:

```json
{
  "city": "Tokyo",
  "population": 14226056
}
```

## Configuring the Model Provider

Set the model client's config in the client's `.toml` file. For example, to
call OpenAI through `chat-completions-client`:

```toml
config.provider = "openai"
config.model = "gpt-4o-mini"
config.api-key = "${process:env:OPENAI_API_KEY}"
```

Without `base-url`, `chat-completions-client` calls `https://api.openai.com/v1`
and `anthropic-messages-client` calls `https://api.anthropic.com/v1`. See
[components/agents](../../components/agents) for each client's config.

> [!NOTE]
>
> When configuring a model that requires it, the api-key is passed within the
> generated config component. That will change once secret management support
> is available (on the roadmap). This project is still in an early development
> phase, not yet intended for any "real world" usage.
