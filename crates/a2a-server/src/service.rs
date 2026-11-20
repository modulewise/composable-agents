//! The runtime service that serves agents over A2A.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use composable_agents::AgentHost;
use composable_runtime::{ComponentHost, ConfigHandler, Service};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::{self, A2aServerConfig, A2aServerConfigHandler, SharedConfig};
use crate::handler::RequestHandler;
use crate::memory::{InMemoryExchangeStore, InMemoryTaskStore};
use crate::server::A2aServer;

// A server with the stores every server uses, which are in memory.
type Server = A2aServer<InMemoryTaskStore, InMemoryExchangeStore>;

/// Starts an A2A server for each `[server.*]` definition where
/// `type = "a2a"`, or one default server when there is none.
pub struct A2aService {
    config: SharedConfig,
    component_host: Mutex<Option<Arc<dyn ComponentHost>>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for A2aService {
    fn default() -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            config: SharedConfig::default(),
            component_host: Mutex::new(None),
            shutdown_tx,
            shutdown_rx,
            tasks: Mutex::new(Vec::new()),
        }
    }
}

impl Service for A2aService {
    fn config_handler(&self) -> Option<Box<dyn ConfigHandler>> {
        Some(Box::new(A2aServerConfigHandler::new(Arc::clone(
            &self.config,
        ))))
    }

    fn set_component_host(&self, component_host: Arc<dyn ComponentHost>) {
        *self.component_host.lock().unwrap() = Some(component_host);
    }

    fn start(&self) -> Result<()> {
        let mut server_configs = std::mem::take(&mut *self.config.lock().unwrap());
        let component_host = self
            .component_host
            .lock()
            .unwrap()
            .clone()
            .expect("set_component_host must be called before start");
        if server_configs.is_empty() {
            let default = config::default_server();
            tracing::info!(
                "No A2A server configured. Starting default on {}:{}.",
                default.host,
                default.port
            );
            server_configs.push(default);
        }

        // Bind every address before building any server, so an address that
        // is taken fails startup with nothing left running.
        let mut listeners = Vec::with_capacity(server_configs.len());
        for server_config in &server_configs {
            let name = &server_config.name;
            let address = format!("{}:{}", server_config.host, server_config.port);
            let address: SocketAddr = address
                .parse()
                .map_err(|e| anyhow!("Server '{name}': invalid address '{address}': {e}"))?;
            let listener = std::net::TcpListener::bind(address)
                .map_err(|e| anyhow!("Server '{name}': cannot bind {address}: {e}"))?;
            listener.set_nonblocking(true)?;
            listeners.push((address, listener));
        }

        // Building a server reads its agent's metadata, which is async. It
        // runs on a thread of its own with its own executor since sync
        // `start` is itself called from within a runtime.
        let servers = {
            let server_configs = server_configs.clone();
            std::thread::Builder::new()
                .name("a2a-build-servers".to_string())
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(async {
                            let mut servers = Vec::new();
                            for server_config in &server_configs {
                                servers.push(server(server_config, &component_host).await?);
                            }
                            Ok::<_, anyhow::Error>(servers)
                        })
                })
                .map_err(|e| anyhow!("could not start a thread to build servers: {e}"))?
                .join()
                .map_err(|_| anyhow!("building servers panicked"))??
        };

        let mut tasks = Vec::new();
        for ((server_config, server), (address, listener)) in
            server_configs.into_iter().zip(servers).zip(listeners)
        {
            let name = server_config.name;
            tracing::info!(
                server_name = name,
                "Starting A2A server on http://{address}"
            );
            let shutdown_rx = self.shutdown_rx.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(error) = server.run(listener, shutdown_rx).await {
                    tracing::error!(server_name = name, "A2A server error: {error}");
                }
            }));
        }
        *self.tasks.lock().unwrap() = tasks;
        Ok(())
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            let _ = self.shutdown_tx.send(true);
            let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
            for task in tasks {
                let _ = task.await;
            }
        })
    }
}

// The server for a definition. A server serves one agent, so the definition
// must select exactly one. Several agents per server are not supported yet.
async fn server(
    server_config: &A2aServerConfig,
    component_host: &Arc<dyn ComponentHost>,
) -> Result<Server> {
    let name = &server_config.name;
    let host = AgentHost::new(
        Arc::clone(component_host),
        server_config.agent_selector.as_ref(),
    );
    let agent = match host.names() {
        [agent] => agent.clone(),
        [] => bail!("Server '{name}' selects no agent"),
        agents => bail!(
            "Server '{name}' selects {} agents ({}), and a server serves exactly one. \
             Set 'agent-selector' to select one.",
            agents.len(),
            agents.join(", ")
        ),
    };
    let handler = RequestHandler::new(
        Arc::new(host),
        agent.clone(),
        InMemoryTaskStore::default(),
        InMemoryExchangeStore::default(),
    )
    .await
    .map_err(|e| anyhow!("Server '{name}': agent '{agent}': {e}"))?;
    A2aServer::new(handler, &server_config.interface_url())
        .map_err(|e| anyhow!("Server '{name}': agent '{agent}': {e}"))
}
