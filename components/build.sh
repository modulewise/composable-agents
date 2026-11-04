#!/bin/sh

set -e

cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown

# Each package that builds a cdylib is a component. The others (such as
# model-client-support) are libraries the components build in.
for project in $(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(any(.targets[]; .crate_types | index("cdylib"))) | .name'); do
  echo "Building $project..."
  cargo build -p "$project" --target "$TARGET" --release

  cargo_name=$(echo "$project" | tr '-' '_')
  core="target/${TARGET}/release/${cargo_name}.wasm"
  wasm-tools component new "$core" -o "lib/${project}.wasm"
  echo "  -> lib/${project}.wasm"
done
