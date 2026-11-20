#!/bin/sh

# Builds the TCK agent as a component, in target/tck-agent.wasm.

set -e

cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown

cargo build --release --target "$TARGET"
wasm-tools component new "target/$TARGET/release/tck_agent.wasm" -o target/tck-agent.wasm
echo "  -> target/tck-agent.wasm"
