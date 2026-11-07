#!/bin/sh

# Lists the agent in the given model's config (gemma via ollama by default),
# then asks it the given question (or default), streaming its answer to stdout
# and its status and usage to stderr. Each model's config is in <model>.toml.

set -e

cd "$(dirname "$0")"

# TEMPORARY: until the components are published to ghcr.io they are pulled from
# the local registry targeted by ../../components/push-to-local-registry.sh
export COMPOSABLE_OCI_REGISTRY_CONFIG="${COMPOSABLE_OCI_REGISTRY_CONFIG:-$(pwd)/../../components/registries.toml}"
export COMPOSABLE_OCI_ALLOW_LATEST="${COMPOSABLE_OCI_ALLOW_LATEST:-1}"

MODEL="${1:-gemma}"
QUESTION="${2:-Why is the sky blue?}"

if [ ! -f "$MODEL.toml" ]; then
  echo "Usage: $0 [claude|gemma|gpt] [question]" >&2
  exit 1
fi

agents list "$MODEL.toml"
agents invoke "$MODEL.toml" -- assistant "$QUESTION"
