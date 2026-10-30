#!/bin/sh

# Asks the model in config.toml for the capital of a place (the first
# argument, or France) with an output schema, and prints its reply as
# JSON that matches the schema.

set -e

cd "$(dirname "$0")"

PLACE="${1:-France}"
PROMPT="What is the capital of $PLACE, and its population?"

SCHEMA='{
  "type": "object",
  "properties": {
    "city": { "type": "string" },
    "population": { "type": "integer" }
  },
  "required": ["city", "population"],
  "additionalProperties": false
}'

composable invoke config.toml -- model.chat "$PROMPT" "$SCHEMA" | jq 'fromjson'
