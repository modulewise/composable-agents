#!/bin/sh

# Builds the calculator component, lists the agent with the given model (gemma
# via ollama by default), then invokes it with the given question (or default),
# streaming its answer to stdout and its status and usage to stderr. The agent
# is in agent.toml, the tools in tools.toml, and each model in <model>.toml.
# With --otel, it loads traced-agent.toml in place of agent.toml and invokes
# traced-assistant, which exports a span for the invocation, with a child span
# for each model call, to an OTLP collector on localhost:4317.

set -e

cd "$(dirname "$0")"

# TEMPORARY: until the components are published to ghcr.io they are pulled from
# the local registry targeted by ../../components/push-to-local-registry.sh
export COMPOSABLE_OCI_REGISTRY_CONFIG="${COMPOSABLE_OCI_REGISTRY_CONFIG:-$(pwd)/../../components/registries.toml}"
export COMPOSABLE_OCI_ALLOW_LATEST="${COMPOSABLE_OCI_ALLOW_LATEST:-1}"

AGENT=assistant
AGENT_CONFIG=agent.toml
if [ "$1" = "--otel" ]; then
  AGENT=traced-assistant
  AGENT_CONFIG=traced-agent.toml
  shift
fi

MODEL="${1:-gemma}"
QUESTION="${2:-What is 17 times 23, minus 5?}"

if [ ! -f "$MODEL.toml" ]; then
  echo "Usage: $0 [--otel] [claude|gemma|gpt] [question]" >&2
  exit 1
fi

wasm-tools parse calculator.wat -o calculator.wasm

agents list "$AGENT_CONFIG" tools.toml "$MODEL.toml"
agents invoke "$AGENT_CONFIG" tools.toml "$MODEL.toml" -- "$AGENT" "$QUESTION"
