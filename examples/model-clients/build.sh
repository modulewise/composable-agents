#!/bin/sh

set -e

cd "$(dirname "$0")"

(cd model && cargo build --quiet --release --target wasm32-unknown-unknown)
mkdir -p lib
wasm-tools component new model/target/wasm32-unknown-unknown/release/model.wasm -o lib/model.wasm
