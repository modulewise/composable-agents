//! The `composable:agents/agent` interface as seen from a host. Lists the
//! components that export it, with their metadata. Supports invoking one of
//! those components with its events and outcome read through typed bindings.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{Result, anyhow, bail};
use composable_runtime::{Component, ComponentHost, Selector};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::PollSender;
use wasmtime::StoreContextMut;
use wasmtime::component::{FutureConsumer, Source, StreamConsumer, StreamResult};

use crate::bindings::BaseAgent;
use crate::bindings::composable::agents::types::{
    AgentError, AgentEvent, AgentRequest, AgentResponse,
};

/// The interface a component exports to be used as an agent. Matching
/// includes the version separator.
pub const AGENT_EXPORT: &str = "composable:agents/agent@";

/// The function key for `metadata` on `composable:agents/agent`.
pub const AGENT_METADATA: &str = "agent.metadata";

// How many events wait for the host before the agent's next write waits.
const EVENT_BUFFER: usize = 16;

/// How an invocation ended, either as the task's outcome, or the agent's
/// refusal to handle the request as a task.
pub type Outcome = Result<AgentResponse, AgentError>;

/// Whether a component exports `composable:agents/agent`.
pub fn exports_agent(component: &Component) -> bool {
    component.metadata.exports.iter().any(|export| {
        export
            .interface_name()
            .is_some_and(|interface| interface.as_str().starts_with(AGENT_EXPORT))
    })
}

/// A view of a [`ComponentHost`] limited to the components that export
/// `composable:agents/agent`, each named by its component name, with typed
/// operations on them.
pub struct AgentHost {
    host: Arc<dyn ComponentHost>,
    agents: Vec<String>,
}

impl AgentHost {
    /// The agents in `host`, limited to those `selector` matches if given.
    pub fn new(host: Arc<dyn ComponentHost>, selector: Option<&Selector>) -> Self {
        let mut agents: Vec<String> = host
            .list_components(selector)
            .into_iter()
            .filter(|component| exports_agent(component))
            .map(|component| component.metadata.name.clone())
            .collect();
        agents.sort();
        Self { host, agents }
    }

    /// The agents' names.
    pub fn names(&self) -> &[String] {
        &self.agents
    }

    /// Each agent's name with its `agent-metadata`, as JSON. An agent's `err`
    /// value is returned as a `ComponentError`.
    pub async fn list(&self) -> Result<Vec<(String, Value)>> {
        let mut listed = Vec::new();
        for agent in &self.agents {
            let metadata = self
                .host
                .invoke(agent, AGENT_METADATA, vec![], None)
                .await?
                .ok_or_else(|| anyhow!("{agent}: {AGENT_METADATA} returned nothing"))?
                .into_json()?;
            listed.push((agent.clone(), metadata));
        }
        Ok(listed)
    }

    /// Invoke `agent` with `request`, handing each event to `on_event` as it
    /// arrives, and return the outcome. When `on_event` returns `false`, the
    /// host stops reading and drops the event stream, which cancels the task.
    /// In that case, the outcome reports the cancellation.
    pub async fn invoke(
        &self,
        agent: &str,
        request: AgentRequest,
        mut on_event: impl FnMut(AgentEvent) -> bool + Send,
    ) -> Result<Outcome> {
        if !self.agents.iter().any(|name| name == agent) {
            bail!("unknown agent '{agent}'");
        }
        let mut instance = self.host.instantiate(agent, None).await?;
        instance
            .run_concurrent(async move |accessor, instance| -> Result<Outcome> {
                let bindings = accessor.with(|mut store| BaseAgent::new(&mut store, &instance))?;
                let stream = match bindings
                    .composable_agents_agent()
                    .call_invoke(accessor, request)
                    .await?
                {
                    Ok(stream) => stream,
                    Err(error) => return Ok(Err(error)),
                };

                let (events_tx, mut events_rx) = mpsc::channel(EVENT_BUFFER);
                let (outcome_tx, outcome_rx) = oneshot::channel();
                accessor.with(|mut store| -> Result<()> {
                    stream
                        .events
                        .pipe(&mut store, EventSink(PollSender::new(events_tx)))?;
                    stream
                        .result
                        .pipe(&mut store, OutcomeSlot(Some(outcome_tx)))?;
                    Ok(())
                })?;
                // The agent runs only while this closure does, so it reads the
                // events to their end and the outcome here.
                while let Some(event) = events_rx.recv().await {
                    if !on_event(event) {
                        break;
                    }
                }
                drop(events_rx);
                Ok(outcome_rx.await?)
            })
            .await?
    }
}

// Takes one event per free slot in a bounded channel. While the channel is
// full it returns `Pending`, so the agent's write waits.
struct EventSink(PollSender<AgentEvent>);

impl<D> StreamConsumer<D> for EventSink {
    type Item = AgentEvent;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<'_, AgentEvent>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        match self.0.poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                let mut event = None;
                source.read(store, &mut event)?;
                if let Some(event) = event
                    && self.0.send_item(event).is_err()
                {
                    return Poll::Ready(Ok(StreamResult::Dropped));
                }
                Poll::Ready(Ok(StreamResult::Completed))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Ok(StreamResult::Dropped)),
            Poll::Pending if finish => Poll::Ready(Ok(StreamResult::Cancelled)),
            Poll::Pending => Poll::Pending,
        }
    }
}

// Hands the outcome to a oneshot.
struct OutcomeSlot(Option<oneshot::Sender<Outcome>>);

impl<D> FutureConsumer<D> for OutcomeSlot {
    type Item = Outcome;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<'_, Outcome>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<()>> {
        let mut value = None;
        source.read(store, &mut value)?;
        if let (Some(value), Some(sender)) = (value, self.0.take()) {
            let _ = sender.send(value);
        }
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::future::Future;

    use composable_runtime::{
        ComponentError, ComponentInstance, ComponentInvoker, ComponentMetadata, Export, Interface,
        InterfaceName, Val,
    };
    use serde_json::json;

    use super::*;
    use crate::bindings::composable::agents::types::{Message, Role};

    // A testing host for components with no bytes. Each invocation returns the
    // value or `err` value set for its component and function.
    #[derive(Default)]
    struct TestHost {
        components: HashMap<String, Component>,
        returns: HashMap<(String, String), Result<Value, Value>>,
    }

    impl TestHost {
        fn component(mut self, name: &str, exports: &[&str]) -> Self {
            self.components
                .insert(name.to_string(), component(name, exports));
            self
        }

        fn returns(mut self, component: &str, function: &str, value: Value) -> Self {
            self.returns
                .insert((component.to_string(), function.to_string()), Ok(value));
            self
        }

        fn fails(mut self, component: &str, function: &str, error: Value) -> Self {
            self.returns
                .insert((component.to_string(), function.to_string()), Err(error));
            self
        }
    }

    impl ComponentInvoker for TestHost {
        fn invoke<'a>(
            &'a self,
            component_name: &'a str,
            function_name: &'a str,
            _args: Vec<Val>,
            _env: Option<HashMap<String, String>>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Val>>> + Send + 'a>> {
            Box::pin(async move {
                let key = (component_name.to_string(), function_name.to_string());
                match self.returns.get(&key) {
                    Some(Ok(value)) => Ok(Some(Val::Json(value.clone()))),
                    Some(Err(error)) => Err(ComponentError {
                        value: Some(Val::Json(error.clone())),
                    }
                    .into()),
                    None => bail!("no return set for {component_name}: {function_name}"),
                }
            })
        }
    }

    impl ComponentHost for TestHost {
        fn get_component(&self, name: &str) -> Option<&Component> {
            self.components.get(name)
        }

        fn list_components(&self, selector: Option<&Selector>) -> Vec<&Component> {
            self.components
                .values()
                .filter(|c| selector.is_none_or(|s| s.matches(&c.metadata.to_selectable())))
                .collect()
        }

        // Its components have no bytes, so none can be instantiated.
        fn instantiate<'a>(
            &'a self,
            component_name: &'a str,
            _env: Option<HashMap<String, String>>,
        ) -> Pin<Box<dyn Future<Output = Result<ComponentInstance>> + Send + 'a>> {
            Box::pin(async move { bail!("{component_name} has no bytes to instantiate") })
        }
    }

    const AGENT: &str = "composable:agents/agent@0.1.0";
    const MODEL_CLIENT: &str = "composable:agents/model-client@0.1.0";

    fn component(name: &str, exports: &[&str]) -> Component {
        Component {
            metadata: ComponentMetadata {
                name: name.to_string(),
                namespace: None,
                package: None,
                labels: HashMap::new(),
                dependents: Some(Vec::new()),
                exports: exports
                    .iter()
                    .map(|interface| Export {
                        name: interface.to_string(),
                        interface: Some(Interface {
                            name: Some(InterfaceName::parse(interface).unwrap()),
                        }),
                    })
                    .collect(),
            },
            functions: HashMap::new(),
        }
    }

    fn metadata(name: &str) -> Value {
        json!({ "name": name, "description": format!("The {name} agent.") })
    }

    fn request() -> AgentRequest {
        AgentRequest {
            message: Message {
                message_id: "message-1".to_string(),
                context_id: None,
                task_id: None,
                role: Role::User,
                parts: Vec::new(),
                metadata: None,
                extensions: Vec::new(),
                reference_task_ids: Vec::new(),
            },
            task_id: None,
            context_id: None,
            accepted_output_modes: Vec::new(),
            requested_extensions: Vec::new(),
            metadata: None,
            task_status: None,
            related_tasks: Vec::new(),
            history: Vec::new(),
            token_budget: None,
        }
    }

    #[tokio::test]
    async fn list_includes_only_the_components_that_export_the_agent_interface() {
        let host = AgentHost::new(
            Arc::new(
                TestHost::default()
                    .component("helper", &[AGENT])
                    .component("assistant", &[AGENT])
                    .component("local", &[MODEL_CLIENT])
                    .returns("helper", AGENT_METADATA, metadata("helper"))
                    .returns("assistant", AGENT_METADATA, metadata("assistant")),
            ),
            None,
        );
        assert_eq!(host.names(), ["assistant", "helper"]);
        assert_eq!(
            host.list().await.unwrap(),
            [
                ("assistant".to_string(), metadata("assistant")),
                ("helper".to_string(), metadata("helper")),
            ]
        );
    }

    #[tokio::test]
    async fn an_agent_that_fails_its_metadata_fails_the_list() {
        let host = AgentHost::new(
            Arc::new(TestHost::default().component("helper", &[AGENT]).fails(
                "helper",
                AGENT_METADATA,
                json!("no 'name' configured"),
            )),
            None,
        );
        let error = host.list().await.unwrap_err();
        let ComponentError {
            value: Some(Val::Json(value)),
        } = error.downcast::<ComponentError>().unwrap()
        else {
            panic!("expected a JSON err value");
        };
        assert_eq!(value, json!("no 'name' configured"));
    }

    #[tokio::test]
    async fn invoking_a_component_that_is_not_an_agent_fails() {
        let host = AgentHost::new(
            Arc::new(TestHost::default().component("local", &[MODEL_CLIENT])),
            None,
        );
        let error = host
            .invoke("local", request(), |_| true)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(error, "unknown agent 'local'");
    }
}
