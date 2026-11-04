#!/bin/sh

# Asks a model, through the given client (chat-completions by default), for the
# capital of a place (the second argument, or France) with an output schema,
# and prints its reply as JSON that matches the schema. Each client's config is
# in <client>.toml.

set -e

cd "$(dirname "$0")"

CLIENT="${1:-chat-completions}"
PLACE="${2:-France}"

if [ ! -f "$CLIENT.toml" ]; then
  echo "Usage: $0 [chat-completions|anthropic-messages] [place]" >&2
  exit 1
fi

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

composable invoke "$CLIENT.toml" -- model.chat "$PROMPT" "$SCHEMA" | jq 'fromjson'
