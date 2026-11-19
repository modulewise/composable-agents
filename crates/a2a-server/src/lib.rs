//! An A2A server for agents that export `composable:agents/agent`. It
//! implements A2A's JSON-RPC binding, converting between A2A's JSON
//! (`a2a-json`) and the agent interface's types, which mirror A2A's.

mod convert;
