//! Graph processing of `[model]` and `[agent]` config with `AgentService`.

use std::io::Write;

use anyhow::Result;
use composable_agents::AgentService;
use composable_runtime::composition::graph::Node;
use composable_runtime::{ComponentGraph, Service};

/// The graph built from `toml`, written to a temp file named for `test`.
fn graph(test: &str, toml: &str) -> Result<ComponentGraph> {
    let mut file = tempfile::Builder::new()
        .prefix(&format!("composable-agents-{test}-"))
        .suffix(".toml")
        .tempfile()?;
    file.write_all(toml.as_bytes())?;
    let handler = AgentService.config_handler().expect("a config handler");
    ComponentGraph::builder()
        .from_path(file.path())
        .add_handler(handler)
        .build()
}

/// The names of the graph's components and of its capabilities, each sorted.
fn names(graph: &ComponentGraph) -> (Vec<&str>, Vec<&str>) {
    let mut components = Vec::new();
    let mut capabilities = Vec::new();
    for node in graph.nodes() {
        match &node.weight {
            Node::Component(definition) => components.push(definition.name.as_str()),
            Node::Capability(definition) => capabilities.push(definition.name.as_str()),
        }
    }
    components.sort();
    capabilities.sort();
    (components, capabilities)
}

#[test]
fn agents_sharing_a_model_define_components_and_capabilities() -> Result<()> {
    let graph = graph(
        "define",
        r#"
        [model.local]
        provider = "ollama"
        name = "gpt-oss:20b"

        [agent.helper]
        model = "local"
        description = "Helps"
        version = "0.1.0"

        [agent.assistant]
        model = "local"
        instructions = "Assist."
        description = "Assists"
        version = "0.1.0"
        "#,
    )?;

    let (components, capabilities) = names(&graph);
    assert_eq!(components, ["_http-client", "assistant", "helper", "local"]);
    assert_eq!(capabilities, ["_http", "_random"]);
    Ok(())
}

#[test]
fn an_agent_rejects_a_property_it_does_not_claim() {
    let error = graph(
        "unclaimed",
        r#"
        [model.local]
        provider = "ollama"
        name = "gpt-oss:20b"

        [agent.helper]
        model = "local"
        description = "Helps"
        version = "0.1.0"
        temperature = 0.5
        "#,
    )
    .expect_err("an unknown property")
    .to_string();
    assert!(error.contains("unknown property 'temperature'"), "{error}");
}

#[test]
fn a_tool_calling_agent_imports_its_toolset() -> Result<()> {
    let graph = graph(
        "toolset",
        r#"
        [model.local]
        provider = "ollama"
        name = "gpt-oss:20b"

        [component.tools]
        uri = "oci://localhost:5001/example/tools:latest"

        [agent.helper]
        model = "local"
        toolset = "tools"
        description = "Helps"
        version = "0.1.0"
        "#,
    )?;
    let (components, capabilities) = names(&graph);
    assert_eq!(components, ["_http-client", "helper", "local", "tools"]);
    assert_eq!(capabilities, ["_http", "_random"]);
    Ok(())
}
