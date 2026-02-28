use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

// ─── Annotation / Finding ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u32,
    pub file: String,
    pub line: u32,
    pub message: String,
    /// "warning" | "error" | "info" | "success"
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: u32,
    pub message: String,
    /// "info" | "warning" | "bug" | "fixed"
    pub level: String,
    pub timestamp: String,
}

use dap_types::{InitializeArgs, WsEnvelope};

use crate::dap::adapter::Adapter;
use crate::dap::client::DapClient;
use crate::home::DebugiumHome;
use crate::server::hub::Hub;

// ─── SessionMeta ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub program: PathBuf,
    pub adapter_id: String,
    pub adapter_pid: Option<u32>,
    pub cwd: PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub port: u16,
}

// ─── Session ──────────────────────────────────────────────────────────────────

pub struct Session {
    pub id: String,
    pub client: Arc<DapClient>,
    pub adapter: Adapter,
    pub meta: RwLock<Option<SessionMeta>>,
    /// Resolves when the adapter sends the `initialized` event (signals config is safe).
    initialized_rx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// Capabilities returned by the adapter's `initialize` response.
    capabilities: RwLock<Value>,
    /// Breakpoints currently set per file: path → verified line numbers.
    pub breakpoints: RwLock<HashMap<String, Vec<u32>>>,
    /// Annotations added by the LLM (file:line → note).
    pub annotations: RwLock<Vec<Annotation>>,
    /// Findings / conclusions left by the LLM.
    pub findings: RwLock<Vec<Finding>>,
    /// Increments on every `stopped` event — lets tools await the next pause.
    pub stopped_tx: Arc<tokio::sync::watch::Sender<u32>>,
    annotation_counter: AtomicU32,
    finding_counter: AtomicU32,
}

impl Session {
    /// Returns true if the adapter declared support for the given capability key.
    pub async fn supports(&self, cap: &str) -> bool {
        self.capabilities.read().await
            .get(cap)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

impl Session {
    pub async fn new(id: String, mut adapter: Adapter, hub: Arc<Hub>) -> Result<Arc<Self>> {
        let mut child = adapter.spawn().context("spawning adapter")?;


        let (event_tx, event_rx) = mpsc::channel::<Value>(256);
        // DapClient::new returns Arc<DapClient>
        let client = DapClient::new(&mut child, event_tx).context("creating DAP client")?;

        let (init_tx, init_rx) = tokio::sync::oneshot::channel::<()>();

        // Pre-register the session so broadcast() works before any WS client connects.
        hub.register(&id).await;

        let (stopped_tx, _) = tokio::sync::watch::channel(0u32);
        let stopped_tx = Arc::new(stopped_tx);

        let session = Arc::new(Session {
            id: id.clone(),
            client: client.clone(),
            adapter,
            meta: RwLock::new(None),
            initialized_rx: tokio::sync::Mutex::new(Some(init_rx)),
            capabilities: RwLock::new(Value::Null),
            breakpoints: RwLock::new(HashMap::new()),
            annotations: RwLock::new(Vec::new()),
            findings: RwLock::new(Vec::new()),
            stopped_tx: stopped_tx.clone(),
            annotation_counter: AtomicU32::new(0),
            finding_counter: AtomicU32::new(0),
        });

        // Spawn event dispatcher: handles initialized signal + enriches stopped events
        {
            let hub2 = hub.clone();
            let client2 = client.clone();
            let session_id = id.clone();
            let stopped_tx2 = stopped_tx.clone();
            let mut event_rx = event_rx;
            let mut init_tx_opt = Some(init_tx);

            tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    // Intercept `initialized` — fire the oneshot, don't broadcast
                    if event.get("type").and_then(Value::as_str) == Some("event")
                        && event.get("event").and_then(Value::as_str) == Some("initialized")
                    {
                        if let Some(tx) = init_tx_opt.take() {
                            let _ = tx.send(());
                        }
                        continue;
                    }

                    // Broadcast every other event to WebSocket clients
                    broadcast_json(&hub2, &session_id, event.clone()).await;

                    // On `stopped`: notify waiters + auto-chain data enrichment
                    if event.get("type").and_then(Value::as_str) == Some("event")
                        && event.get("event").and_then(Value::as_str) == Some("stopped")
                    {
                        stopped_tx2.send_modify(|n| *n = n.wrapping_add(1));
                        let thread_id = event
                            .get("body")
                            .and_then(|b| b.get("threadId"))
                            .and_then(Value::as_u64)
                            .unwrap_or(1) as u32;
                        let reason = event.get("body")
                            .and_then(|b| b.get("reason"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        enrich_stopped(&hub2, &client2, &session_id, thread_id, &reason).await;
                    }
                }
            });
        }

        // Send initialize — store capabilities from the response
        let args = serde_json::to_value(InitializeArgs {
            adapter_id: session.adapter.adapter_id().to_string(),
            ..Default::default()
        })?;
        if let Ok(init_resp) = session.client.request("initialize", Some(args)).await {
            if let Some(caps) = init_resp.get("body") {
                *session.capabilities.write().await = caps.clone();
            }
        }
        info!("[{id}] initialized");

        Ok(session)
    }

    /// Connect to an already-running DAP server over TCP (Metals, Java debug server, etc.)
    /// and run the initialize handshake. Does NOT send `launch` — caller calls `configure_and_attach`.
    pub async fn from_tcp(
        id: String,
        tcp_addr: std::net::SocketAddr,
        adapter: Adapter,
        hub: Arc<Hub>,
    ) -> Result<Arc<Self>> {
        use crate::dap::client::DapClient;
        let stream = tokio::net::TcpStream::connect(tcp_addr).await
            .with_context(|| format!("connecting to DAP server at {tcp_addr}"))?;

        let (event_tx, event_rx) = mpsc::channel::<Value>(256);
        let client = DapClient::from_tcp(stream, event_tx);

        let (init_tx, init_rx) = tokio::sync::oneshot::channel::<()>();
        hub.register(&id).await;

        let (stopped_tx, _) = tokio::sync::watch::channel(0u32);
        let stopped_tx = Arc::new(stopped_tx);

        let session = Arc::new(Session {
            id: id.clone(),
            client: client.clone(),
            adapter,
            meta: RwLock::new(None),
            initialized_rx: tokio::sync::Mutex::new(Some(init_rx)),
            capabilities: RwLock::new(Value::Null),
            breakpoints: RwLock::new(HashMap::new()),
            annotations: RwLock::new(Vec::new()),
            findings: RwLock::new(Vec::new()),
            stopped_tx: stopped_tx.clone(),
            annotation_counter: AtomicU32::new(0),
            finding_counter: AtomicU32::new(0),
        });

        // Event dispatcher
        {
            let hub2 = hub.clone();
            let client2 = client.clone();
            let session_id = id.clone();
            let stopped_tx2 = stopped_tx.clone();
            let mut event_rx = event_rx;
            let mut init_tx_opt = Some(init_tx);
            tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    if event.get("type").and_then(Value::as_str) == Some("event")
                        && event.get("event").and_then(Value::as_str) == Some("initialized")
                    {
                        if let Some(tx) = init_tx_opt.take() { let _ = tx.send(()); }
                        continue;
                    }
                    broadcast_json(&hub2, &session_id, event.clone()).await;
                    if event.get("event").and_then(Value::as_str) == Some("stopped") {
                        stopped_tx2.send_modify(|n| *n = n.wrapping_add(1));
                        let thread_id = event.get("body").and_then(|b| b.get("threadId"))
                            .and_then(Value::as_u64).unwrap_or(1) as u32;
                        let reason = event.get("body").and_then(|b| b.get("reason"))
                            .and_then(Value::as_str).unwrap_or("").to_string();
                        enrich_stopped(&hub2, &client2, &session_id, thread_id, &reason).await;
                    }
                }
            });
        }

        let args = serde_json::to_value(InitializeArgs {
            adapter_id: session.adapter.adapter_id().to_string(),
            ..Default::default()
        })?;
        if let Ok(init_resp) = session.client.request("initialize", Some(args)).await {
            if let Some(caps) = init_resp.get("body") {
                *session.capabilities.write().await = caps.clone();
            }
        }
        info!("[{id}] tcp-initialized from {tcp_addr}");

        Ok(session)
    }

    /// Full DAP handshake used at startup:
    ///   launch (fire & forget) → wait for `initialized` event → setBreakpoints → configurationDone
    ///
    /// Note: debugpy (and most adapters) only send the `launch` response *after*
    /// `configurationDone`, so awaiting the launch response before sending the
    /// configuration sequence would deadlock.
    pub async fn configure_and_launch(
        &self,
        program: PathBuf,
        cwd: PathBuf,
        breakpoints: &[(String, Vec<u32>)],
    ) -> Result<()> {
        // Populate session metadata
        let adapter_id = self.adapter.adapter_id().to_string();
        let adapter_pid = self.adapter.process.as_ref().map(|p| p.pid);
        let meta = SessionMeta {
            program: program.clone(),
            adapter_id: adapter_id.clone(),
            adapter_pid,
            cwd: cwd.clone(),
            started_at: chrono::Utc::now(),
            port: 0, // will be enriched by main.rs if needed
        };
        *self.meta.write().await = Some(meta.clone());

        // Write info.json to ~/.debugium/sessions/<id>/info.json
        if let Ok(home) = DebugiumHome::open() {
            if let Ok(session_dir) = home.ensure_session_dir(&self.id) {
                let info_path = session_dir.join("info.json");
                if let Ok(json) = serde_json::to_string_pretty(&meta) {
                    tokio::fs::write(&info_path, json).await.ok();
                }
            }
        }

        // 1. launch — fire and forget; response arrives only after configurationDone
        let launch_args = self.adapter.launch_args(&program, &cwd);
        self.client.notify("launch", Some(launch_args)).await?;

        // 2. wait for `initialized` event (10s timeout)
        let mut guard = self.initialized_rx.lock().await;
        if let Some(rx) = guard.take() {
            tokio::time::timeout(tokio::time::Duration::from_secs(10), rx)
                .await
                .map_err(|_| anyhow::anyhow!("Timed out waiting for initialized event"))?
                .map_err(|_| anyhow::anyhow!("initialized channel closed"))?;
        }
        drop(guard);

        // 3. setBreakpoints per file
        for (file, lines) in breakpoints {
            if let Err(e) = self.set_breakpoints(file, lines.clone()).await {
                warn!("setBreakpoints {file}: {e}");
            }
        }

        // 4. setExceptionBreakpoints — stop on uncaught exceptions by default
        //    Only send if the adapter declares exceptionBreakpointFilters capability.
        let has_exc_filters = self.capabilities.read().await
            .get("exceptionBreakpointFilters")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if has_exc_filters {
            if let Err(e) = self.client.request("setExceptionBreakpoints", Some(serde_json::json!({
                "filters": ["uncaught"]
            }))).await {
                warn!("setExceptionBreakpoints: {e}");
            }
        }

        // 5. configurationDone
        self.client.request("configurationDone", None).await?;
        info!("[{}] configuration done — target running", self.id);
        Ok(())
    }

    /// Return the stored adapter capabilities.
    pub async fn get_capabilities(&self) -> Value {
        self.capabilities.read().await.clone()
    }

    /// Add an annotation (file:line note left by the LLM).
    pub async fn add_annotation(&self, file: String, line: u32, message: String, color: String) -> Annotation {
        let id = self.annotation_counter.fetch_add(1, Ordering::SeqCst);
        let ann = Annotation { id, file, line, message, color };
        self.annotations.write().await.push(ann.clone());
        ann
    }

    /// Add a finding (conclusion left by the LLM).
    pub async fn add_finding(&self, message: String, level: String) -> Finding {
        let id = self.finding_counter.fetch_add(1, Ordering::SeqCst);
        let f = Finding {
            id,
            message,
            level,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.findings.write().await.push(f.clone());
        f
    }

    /// Wait for the next `stopped` event (with a timeout in seconds).
    pub async fn wait_for_stop(&self, timeout_secs: u64) -> Result<()> {
        let mut rx = self.stopped_tx.subscribe();
        let before = *rx.borrow_and_update();
        tokio::time::timeout(tokio::time::Duration::from_secs(timeout_secs), async move {
            loop {
                if rx.changed().await.is_err() { break; }
                if *rx.borrow() != before { break; }
            }
        }).await.map_err(|_| anyhow::anyhow!("timeout waiting for stopped event after {}s", timeout_secs))
    }

    /// Set breakpoints on a running session (e.g. from the UI or MCP).
    /// Stores verified lines in `self.breakpoints`.
    pub async fn set_breakpoints(&self, file: &str, lines: Vec<u32>) -> Result<Value> {
        let args = serde_json::json!({
            "source": { "path": file },
            "breakpoints": lines.iter().map(|l| serde_json::json!({ "line": l })).collect::<Vec<_>>()
        });
        let resp = self.client.request("setBreakpoints", Some(args)).await?;
        // Extract verified lines from the DAP response
        let verified: Vec<u32> = resp.get("body")
            .and_then(|b| b.get("breakpoints"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter()
                .filter_map(|b| b.get("line").and_then(Value::as_u64).map(|l| l as u32))
                .collect())
            .unwrap_or_else(|| lines.clone());
        let mut bps = self.breakpoints.write().await;
        if verified.is_empty() {
            bps.remove(file);
        } else {
            bps.insert(file.to_string(), verified);
        }
        Ok(resp)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

async fn broadcast_json(hub: &Hub, session_id: &str, msg: Value) {
    let envelope = WsEnvelope { session_id: session_id.to_string(), msg };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

/// On `stopped`: chain threads → stackTrace → scopes → variables, then push source content.
/// If `reason` is "exception", also call `exceptionInfo` and broadcast as a synthetic event.
async fn enrich_stopped(hub: &Hub, client: &Arc<DapClient>, session_id: &str, thread_id: u32, reason: &str) {
    // 1. threads
    let threads_resp = match client.request("threads", None).await {
        Ok(v) => { broadcast_json(hub, session_id, v.clone()).await; v }
        Err(e) => { warn!("threads failed: {e}"); return; }
    };
    let _ = threads_resp; // used to confirm request succeeded

    // 2. stackTrace
    let stack_args = serde_json::json!({
        "threadId": thread_id,
        "startFrame": 0,
        "levels": 20
    });
    let stack_resp = match client.request("stackTrace", Some(stack_args)).await {
        Ok(v) => { broadcast_json(hub, session_id, v.clone()).await; v }
        Err(e) => { warn!("stackTrace failed: {e}"); return; }
    };

    // Extract top frame info
    let frames = stack_resp
        .get("body").and_then(|b| b.get("stackFrames"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let top = frames.first();
    let frame_id = top.and_then(|f| f.get("id")).and_then(Value::as_u64).unwrap_or(1) as u32;
    let source_path = top
        .and_then(|f| f.get("source"))
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let source_line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;

    // 3. scopes
    let scopes_resp = match client.request("scopes", Some(serde_json::json!({ "frameId": frame_id }))).await {
        Ok(v) => { broadcast_json(hub, session_id, v.clone()).await; v }
        Err(e) => { warn!("scopes failed: {e}"); return; }
    };

    // 4. variables for each scope
    if let Some(scopes) = scopes_resp.get("body").and_then(|b| b.get("scopes")).and_then(Value::as_array) {
        for scope in scopes {
            let ref_ = scope.get("variablesReference").and_then(Value::as_u64).unwrap_or(0);
            if ref_ == 0 { continue; }
            let vars_args = serde_json::json!({ "variablesReference": ref_ });
            match client.request("variables", Some(vars_args)).await {
                Ok(v) => broadcast_json(hub, session_id, v).await,
                Err(e) => warn!("variables({ref_}) failed: {e}"),
            }
        }
    }

    // 5. exceptionInfo — if stopped on an exception, fetch and broadcast details
    if reason == "exception" {
        match client.request("exceptionInfo", Some(serde_json::json!({ "threadId": thread_id }))).await {
            Ok(resp) => {
                let synthetic = WsEnvelope {
                    session_id: session_id.to_string(),
                    msg: serde_json::json!({
                        "type": "event",
                        "event": "exceptionInfo",
                        "body": resp.get("body").cloned().unwrap_or(Value::Null)
                    }),
                };
                if let Ok(json) = serde_json::to_string(&synthetic) {
                    hub.broadcast(session_id, json).await;
                }
            }
            Err(e) => warn!("exceptionInfo failed: {e}"),
        }
    }

    // 6. Read source file and push as synthetic `sourceLoaded` event
    if let Some(path) = source_path {
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let synthetic = WsEnvelope {
                    session_id: session_id.to_string(),
                    msg: serde_json::json!({
                        "type": "event",
                        "event": "sourceLoaded",
                        "body": {
                            "path": path,
                            "lines": lines,
                            "currentLine": source_line,
                        }
                    }),
                };
                if let Ok(json) = serde_json::to_string(&synthetic) {
                    hub.broadcast(session_id, json).await;
                }
            }
            Err(e) => warn!("source read failed for {path}: {e}"),
        }
    }
}

// ─── SessionRegistry ──────────────────────────────────────────────────────────

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
