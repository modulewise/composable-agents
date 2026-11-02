//! Typed bindings to an agent component's `composable:agents/agent` export,
//! whose `invoke` returns a stream and a future that only typed bindings can
//! read. The world's `model-client` import is satisfied by the composition.

wasmtime::component::bindgen!({
    path: "components/agents/wit",
    world: "composable:agents/base-agent",
});
