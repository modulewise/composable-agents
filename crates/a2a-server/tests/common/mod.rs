//! A request handler for the test agent in `tests/test-agent`. The test agent
//! is built as a component the first time a test needs it, which requires the
//! `wasm32-unknown-unknown` target and `wasm-tools`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use composable_a2a_server::{InMemoryExchangeStore, InMemoryTaskStore, RequestHandler};
use composable_agents::AgentHost;
use composable_runtime::Runtime;

pub type Handler = RequestHandler<InMemoryTaskStore, InMemoryExchangeStore>;

/// A request handler for the test agent that stores its tasks in `tasks`,
/// with the runtime that hosts the agent.
pub async fn test_agent_handler(tasks: InMemoryTaskStore) -> (Handler, Runtime) {
    // The component's name is its file's name without the extension.
    let runtime = Runtime::builder()
        .from_paths(&[test_agent().to_path_buf()])
        .build()
        .await
        .unwrap();
    runtime.start().unwrap();
    let host = Arc::new(AgentHost::new(runtime.host(), None));
    let handler = RequestHandler::new(host, "test-agent", tasks, InMemoryExchangeStore::default())
        .await
        .unwrap();
    (handler, runtime)
}

// The test agent as a component, built once.
fn test_agent() -> &'static Path {
    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    COMPONENT.get_or_init(|| {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test-agent");
        let target = "wasm32-unknown-unknown";
        run(Command::new("cargo")
            .args(["build", "--release", "--target", target])
            .current_dir(&package));
        let core = package.join(format!("target/{target}/release/test_agent.wasm"));
        let component = package.join("target/test-agent.wasm");
        run(Command::new("wasm-tools")
            .args(["component", "new"])
            .arg(&core)
            .arg("-o")
            .arg(&component));
        component
    })
}

fn run(command: &mut Command) {
    let status = command.status().expect("the command starts");
    assert!(status.success(), "{command:?} failed");
}
