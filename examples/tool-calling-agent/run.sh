#!/bin/sh

# Builds the calculator component, lists the agent with the given model (gemma
# via ollama by default), then asks it the given question (or default),
# streaming its answer to stdout and its status and usage to stderr. The agent
# is in agent.toml, the tools in tools.toml, and each model in <model>.toml.

set -e

cd "$(dirname "$0")"

# TEMPORARY: until the components are published to ghcr.io they are pulled from
# the local registry targeted by ../../components/push-to-local-registry.sh
export COMPOSABLE_OCI_REGISTRY_CONFIG="${COMPOSABLE_OCI_REGISTRY_CONFIG:-$(pwd)/../../components/registries.toml}"
export COMPOSABLE_OCI_ALLOW_LATEST="${COMPOSABLE_OCI_ALLOW_LATEST:-1}"

MODEL="${1:-gemma}"
QUESTION="${2:-What is 17 times 23, minus 5?}"

if [ ! -f "$MODEL.toml" ]; then
  echo "Usage: $0 [claude|gemma|gpt] [question]" >&2
  exit 1
fi

wasm-tools parse calculator.wat -o calculator.wasm

agents list agent.toml tools.toml "$MODEL.toml"
agents invoke agent.toml tools.toml "$MODEL.toml" -- assistant "$QUESTION"
