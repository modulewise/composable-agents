#!/bin/sh

# Serves the agent of another example on an A2A server, and calls it.
#
#   serve [--otel] <agent> [model]  serve the agent of ../<agent> on port 4001
#   card                            print the agent's card
#   send <message>                  send a message and print the response
#   stream <message>                send a message and print the event stream
#   call <message> [server-url]     invoke the caller agent, which calls the
#                                   server through an a2a-tool
#
# <agent> is chat-agent or tool-calling-agent. With --otel, the server serves
# the traced tool-calling-agent, which exports spans to an OTLP collector on
# localhost:4317. The model is gemma via ollama unless one is given, or MODEL
# is set. card, send and stream use A2A_SERVER_URL, or this example's server.

set -e

cd "$(dirname "$0")"
EXAMPLE="$(pwd)"

# TEMPORARY: until the components are published to ghcr.io they are pulled from
# the local registry targeted by ../../components/push-to-local-registry.sh
export COMPOSABLE_OCI_REGISTRY_CONFIG="${COMPOSABLE_OCI_REGISTRY_CONFIG:-$EXAMPLE/../../components/registries.toml}"
export COMPOSABLE_OCI_ALLOW_LATEST="${COMPOSABLE_OCI_ALLOW_LATEST:-1}"

URL="${A2A_SERVER_URL:-http://127.0.0.1:4001}"
MODEL="${MODEL:-gemma}"

usage() {
  sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 1
}

# Sends a message with the given method, as a JSON-RPC request.
rpc() {
  BODY=$(jq -n --arg method "$1" --arg text "$2" --arg id "$(uuidgen)" '{
    jsonrpc: "2.0",
    id: 1,
    method: $method,
    params: { message: { messageId: $id, role: "ROLE_USER", parts: [{ text: $text }] } }
  }')
  curl -sN "$URL" -H "Content-Type: application/json" -H "A2A-Version: 1.0" -d "$BODY"
}

COMMAND="$1"
[ $# -gt 0 ] && shift

case "$COMMAND" in
  serve)
    OTEL=
    if [ "$1" = "--otel" ]; then
      OTEL=1
      shift
    fi
    AGENT="$1"
    MODEL="${2:-$MODEL}"
    case "$AGENT" in
      chat-agent)
        [ -z "$OTEL" ] || usage
        cd ../chat-agent
        [ -f "$MODEL.toml" ] || usage
        exec a2a-server "$MODEL.toml" "$EXAMPLE/server.toml"
        ;;
      tool-calling-agent)
        cd ../tool-calling-agent
        [ -f "$MODEL.toml" ] || usage
        wasm-tools parse calculator.wat -o calculator.wasm
        if [ -n "$OTEL" ]; then
          exec a2a-server traced-agent.toml tools.toml "$MODEL.toml" "$EXAMPLE/traced-server.toml"
        fi
        exec a2a-server agent.toml tools.toml "$MODEL.toml" "$EXAMPLE/server.toml"
        ;;
      *)
        usage
        ;;
    esac
    ;;
  card)
    curl -s "$URL/.well-known/agent-card.json"
    echo
    ;;
  send)
    [ -n "$1" ] || usage
    rpc SendMessage "$1"
    echo
    ;;
  stream)
    [ -n "$1" ] || usage
    rpc SendStreamingMessage "$1"
    ;;
  call)
    [ -n "$1" ] || usage
    [ -z "$2" ] || export A2A_SERVER_URL="$2"
    agents invoke caller.toml "../tool-calling-agent/$MODEL.toml" -- caller "$1"
    ;;
  *)
    usage
    ;;
esac
