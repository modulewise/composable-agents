# composable:a2a

Calling agents on [A2A](https://a2a-protocol.org) servers.

See [`wit/`](wit) for the full definitions.

## Interfaces

- **`types`:** the agent card, with its interfaces, capabilities, security
  schemes and signatures, and the responses an agent streams.
- **`client`:** a client for one agent on an A2A server.

The types follow A2A's `a2a.proto`, field for field, and build on
[`composable:agents/types`](../agents).

## client

**`connect(server-url, extensions)`** fetches the agent's card from
`/.well-known/agent-card.json` under `server-url`, selects the interface to
call, and returns an `agent`. Each request then asks the agent to activate
`extensions` (A2A's `A2A-Extensions`). It fails when the card has no interface
the client supports, or requires an extension that isn't in `extensions`.

An `agent` has:

- **`card()`** and **`interface()`:** the card as fetched, and the interface
  to which requests are sent.
- **`send-message(request)`:** A2A's `SendMessage`. It returns the task that
  handles the message, or a message in response. By default the agent waits
  until the task reaches a terminal or interrupted state. With
  `return-immediately`, it returns the task as soon as it's created.
- **`send-streaming-message(request)`:** A2A's `SendStreamingMessage`,
  preferred when the card's `capabilities.streaming` is true. It returns the
  responses as they occur (a task followed by its status and artifact updates,
  or one message), with a future that resolves when they end.
- **`get-task`**, **`list-tasks`** and **`cancel-task`:** A2A's operations of
  those names.
- **`activated-extensions()`:** the extensions the agent listed as activated
  in its most recent response.

Each request record is A2A's, without `tenant`, which the client takes from
the selected interface.

A failure is reported in one of three ways:

- **Request errors.** The operation returns an `error`, with the JSON-RPC
  `code` and `data` when the agent sent them (e.g. `-32001`, task not found).
- **Task states.** A task whose state is `failed`, `rejected`, or `canceled` is
  a response, not an error. The operation succeeds and returns the task.
- **Stream errors.** The future of `send-streaming-message` resolves to an
  `error` when an event has a JSON-RPC error or isn't valid, or the responses
  end before a message arrives or the task reaches a terminal or interrupted
  state.

## a2a-client

Implements `client` with A2A's JSON-RPC binding over HTTP, for A2A 1.0. It
selects the first `JSONRPC` interface on the card whose `protocol-version` is
`1.0`, and sends `A2A-Version: 1.0` with every request.

- **Required fields are checked.** A card or a response without a field that
  A2A requires, or with an empty list where A2A requires an element, is not
  valid, and the operation fails.
- **Message ids are required.** A2A requires a message's creator to assign its
  id, so the client fails on a message whose `message-id` is empty.
- **No redirect is followed.** A redirect response is an error.
- **Authentication is not supported.** The card's security schemes are read,
  but the client sends no credentials.

It imports `componentized:http/client`, and has no config.

Not yet supported: the gRPC and HTTP+JSON bindings, `SubscribeToTask`, push
notification configs, the extended agent card, caching the card, and verifying
its signatures.
