# tck-agent

An agent for running the [A2A TCK](https://github.com/a2aproject/a2a-tck)
against the A2A server. It has no model. For each message, it does what the
TCK's scenarios specify for the prefix of the message's id. It is built with
`agent-support`, as chat-agent and tool-calling-agent are.

`cargo test` does not build or run it.

## Run

`build.sh` requires the `wasm32-unknown-unknown` target and `wasm-tools`.

From this directory, build the agent and serve it on port 9999:

```sh
./build.sh
a2a-server tck-agent.toml
```

Then run the TCK from a checkout of it, with its dependencies installed as its
README describes:

```sh
./run_tck.py --sut-host http://127.0.0.1:9999 --transport jsonrpc
```

The TCK writes its report to its `reports` directory.

## Failures

| Requirement | TCK result | Reason |
|---|---|---|
| `DM-MSG-001` | failed | The agent always responds with a task. Message responses are not supported. |
| `CORE-HIST-002` | failed | The TCK's JSON-RPC client sends `history_length`, and A2A requires `historyLength` ([a2a-tck#242](https://github.com/a2aproject/a2a-tck/issues/242)). |
| `CORE-HIST-001` | xfailed | The same TCK bug. The TCK marks this test's failure as expected. |

## Slow tests

Each of the three `TestMultiStreamOrdering` tests takes 10 seconds. Each
subscribes to an interrupted task and waits for the TCK's timeout, because the
stream stays open.

## Scenarios not implemented

- `tck-message-response`: the agent always responds with a task.
- `tck-reject-task`: no TCK test sends it.
