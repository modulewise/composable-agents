# Chat Completions Example

Demonstrates calling a model directly through the Chat Completions API. The
`model` component is implemented here as an example entry point (as opposed to
using an agent component that plays that same role). It imports the
`chat-completions-client` component which exports the
`composable:agents/model-client` interface as defined in
[WIT](../../components/agents/wit). Each time the model component's exported
`chat` function is invoked, it opens a session, sends the prompt within a
request, and returns the reply as text. When an output schema is passed, the
reply JSON will match it.

## Prerequisites

- `composable` (composable-runtime's CLI)
- `wasm-tools`
- a rust toolchain with the `wasm32-unknown-unknown` target
- chat-completions-client built: `../../components/build.sh`
- Ollama serving `gpt-oss:20b`, or another model specified in `config.toml`
  (see below)

## Build

```sh
./build.sh
```

## Run

```sh
./run.sh Japan
```

The script asks for the capital of the given place (France by default) and its
population, passing an output schema for the reply.

Output will match the schema:

```json
{
  "city": "Tokyo",
  "population": 14226056
}
```

## Configuring the Model Provider

Set the model client's config in `config.toml`, for example:

```toml
config.provider = "openai"
config.model = "gpt-4o-mini"
config.api-key = "${process:env:OPENAI_API_KEY}"
```

Without `base-url`, the client calls `https://api.openai.com/v1`.

> [!NOTE]
>
> When configuring a model that requires it, the api-key is passed within the
> generated config component. That will change once secret management support
> is available (on the roadmap). This project is still in an early development
> phase, not yet intended for any "real world" usage.
