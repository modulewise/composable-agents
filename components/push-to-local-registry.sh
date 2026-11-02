#!/bin/bash

# Builds the components and pushes them into the local registry.
#
# TEMPORARY: this script will be removed once the components are published to
# ghcr.io, and component URIs will then reference those published versions.
#
# Start the registry first, e.g. composable-runtime's
# `examples/local-registry/start-registry.sh`. When running, point
# `COMPOSABLE_OCI_REGISTRY_CONFIG` at `registries.toml`, and set
# `COMPOSABLE_OCI_ALLOW_LATEST=1` so each run pulls what was last pushed.

set -euo pipefail

cd "$(dirname "$0")"

REGISTRY="localhost:5001"
REPOSITORY="composable-agents"
TAG="latest"
COMPONENTS=(chat-agent chat-completions-client)

if ! command -v wkg &>/dev/null; then
  echo "Error: wkg not found (cargo install wkg)"
  exit 1
fi

if ! curl -s -o /dev/null --max-time 2 "http://$REGISTRY/v2/"; then
  echo "Error: no registry on $REGISTRY"
  exit 1
fi

./build.sh

for component in "${COMPONENTS[@]}"; do
  wkg oci push --insecure "$REGISTRY" "$REGISTRY/$REPOSITORY/$component:$TAG" "lib/$component.wasm"
done
