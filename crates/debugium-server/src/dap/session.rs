use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

use dap_types::{InitializeArgs, WsEnvelope};

use crate::dap::adapter::Adapter;
use crate::dap::client::DapClient;
use crate::server::hub::Hub;

/// A single debugging session — one DAP adapter process + client.
pub struct Session {
    pub id: String,
    pub client: Arc<DapClient>,
    pub adapter: Adapter,
}

impl Session {
    pub async fn new(id: String, mut adapter: Adapter, hub: Arc<Hub>) -> Result<Arc<Self>> {
        let mut child = adapter.spawn().context("spawning adapter")?;

        // Channel for adapter events → session handler
        let (event_tx, mut event_rx) = mpsc::channel::<Value>(256);
        let client = DapClient::new(&mut child, event_tx).context("creating DAP client")?;

        let session = Arc::new(Session { id: id.clone(), client, adapter });

        // Spawn event dispatcher — converts adapter events to WebSocket broadcasts
        let hub2 = hub.clone();
        let session_id = id.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let envelope = WsEnvelope {
                    session_id: session_id.clone(),
                    msg: event,
                };
                if let Ok(json) = serde_json::to_string(&envelope) {
                    hub2.broadcast(&session_id, json).await;
                }
            }
        });

        // Send initialize
        let args = serde_json::to_value(InitializeArgs {
            adapter_id: session.adapter.adapter_id().to_string(),
            ..Default::default()
        })?;
        let _ = session.client.request("initialize", Some(args)).await;
        info!("[{id}] initialized");

        Ok(session)
    }

    pub async fn launch(&self, program: PathBuf, cwd: PathBuf) -> Result<Value> {
        let args = self.adapter.launch_args(&program, &cwd);
        self.client.request("launch", Some(args)).await
    }

    pub async fn set_breakpoints(&self, file: &str, lines: Vec<u32>) -> Result<Value> {
        let args = serde_json::json!({
            "source": { "path": file },
            "breakpoints": lines.iter().map(|l| serde_json::json!({ "line": l })).collect::<Vec<_>>()
        });
        self.client.request("setBreakpoints", Some(args)).await
    }

    pub async fn config_done(&self) -> Result<Value> {
        self.client.request("configurationDone", None).await
    }
}

/// Registry of active debugging sessions.
pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

impl SessionRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
        })
    }

    pub async fn insert(&self, session: Arc<Session>) {
        self.sessions.write().await.insert(session.id.clone(), session);
    }

    pub async fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.read().await.get(id).cloned()
    }

    pub async fn list(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }
}
