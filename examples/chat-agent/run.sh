#!/bin/sh

# Lists the agent in the given provider's config (ollama by default), then asks
# it the given question (or default), streaming its answer to stdout and its
# status and usage to stderr. Each provider's config is in <provider>.toml.

set -e

cd "$(dirname "$0")"

# TEMPORARY: until the components are published to ghcr.io they are pulled from
# the local registry targeted by ../../components/push-to-local-registry.sh
export COMPOSABLE_OCI_REGISTRY_CONFIG="${COMPOSABLE_OCI_REGISTRY_CONFIG:-$(pwd)/../../components/registries.toml}"
export COMPOSABLE_OCI_ALLOW_LATEST="${COMPOSABLE_OCI_ALLOW_LATEST:-1}"

PROVIDER="${1:-ollama}"
QUESTION="${2:-Why is the sky blue?}"

if [ ! -f "$PROVIDER.toml" ]; then
  echo "Usage: $0 [ollama|openai|anthropic] [question]" >&2
  exit 1
fi

agents list "$PROVIDER.toml"
agents invoke "$PROVIDER.toml" -- assistant "$QUESTION"
