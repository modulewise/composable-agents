//! The HTTP server for one agent. It serves the agent's card at the
//! well-known path and the JSON-RPC binding at the root path.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use tokio::sync::watch;

use crate::handler::RequestHandler;
use crate::jsonrpc;
use crate::store::{ExchangeStore, TaskStore};

/// The path of an agent's card.
pub const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

// How long a client may cache the card. The card changes only when the server
// restarts.
const CARD_CACHE_CONTROL: &str = "max-age=300";

/// Serves one agent over HTTP.
pub struct A2aServer<T, E> {
    handler: RequestHandler<T, E>,
    card: Card,
}

// The agent's card as JSON, with the entity tag for it.
struct Card {
    json: Vec<u8>,
    etag: HeaderValue,
}

impl<T: TaskStore, E: ExchangeStore> A2aServer<T, E> {
    /// A server for the handler's agent, whose card gives `url` as the URL of
    /// its JSON-RPC interface. Fails when the agent's metadata has an empty
    /// list where A2A requires at least one element.
    pub fn new(handler: RequestHandler<T, E>, url: &str) -> Result<Self> {
        let card = handler.agent_card(url).map_err(|e| anyhow!(e))?;
        let json = serde_json::to_vec(&card)?;
        let mut hasher = DefaultHasher::new();
        json.hash(&mut hasher);
        let etag = HeaderValue::from_str(&format!("\"{:016x}\"", hasher.finish()))?;
        Ok(A2aServer {
            handler,
            card: Card { json, etag },
        })
    }

    /// The server's routes.
    pub fn router(self) -> axum::Router {
        axum::Router::new()
            .route(AGENT_CARD_PATH, get(Self::card))
            .route("/", post(Self::rpc))
            .with_state(Arc::new(self))
    }

    /// Serves on `listener` until `shutdown` changes.
    pub async fn run(
        self,
        listener: std::net::TcpListener,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        axum::serve(listener, self.router())
            .with_graceful_shutdown(async move {
                let _ = shutdown.changed().await;
            })
            .await?;
        Ok(())
    }

    // The card, or `304 Not Modified` for a request that has its entity tag.
    async fn card(State(server): State<Arc<Self>>, headers: HeaderMap) -> Response {
        let card = &server.card;
        let cached = headers.get(header::IF_NONE_MATCH) == Some(&card.etag);
        let mut response = if cached {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            response
        } else {
            let mut response = Response::new(Body::from(card.json.clone()));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response
        };
        let headers = response.headers_mut();
        headers.insert(header::ETAG, card.etag.clone());
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CARD_CACHE_CONTROL),
        );
        response
    }

    async fn rpc(State(server): State<Arc<Self>>, request: Request) -> Response {
        jsonrpc::handle(&server.handler, request).await
    }
}
