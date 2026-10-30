#!/bin/sh

set -e

cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown

for project in $(cargo metadata --no-deps --format-version 1 | jq -r '.packages[].name'); do
  echo "Building $project..."
  cargo build -p "$project" --target "$TARGET" --release

  cargo_name=$(echo "$project" | tr '-' '_')
  core="target/${TARGET}/release/${cargo_name}.wasm"
  wasm-tools component new "$core" -o "lib/${project}.wasm"
  echo "  -> lib/${project}.wasm"
done
