#!/bin/sh

# Lists the agent in config.toml, then asks it the given question (or default),
# streaming its answer to stdout and its status and usage to stderr.

set -e

cd "$(dirname "$0")"

# TEMPORARY: until the components are published to ghcr.io they are pulled from
# the local registry targeted by ../../components/push-to-local-registry.sh
export COMPOSABLE_OCI_REGISTRY_CONFIG="${COMPOSABLE_OCI_REGISTRY_CONFIG:-$(pwd)/../../components/registries.toml}"
export COMPOSABLE_OCI_ALLOW_LATEST="${COMPOSABLE_OCI_ALLOW_LATEST:-1}"

QUESTION="${1:-Why is the sky blue?}"

agents list config.toml
agents invoke config.toml -- assistant "$QUESTION"
