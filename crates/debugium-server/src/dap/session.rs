use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

// ─── Timeline ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub id: u32,
    pub file: String,
    pub line: u32,
    pub timestamp: String,
    /// name → display value for all locals at this stop
    pub variables_snapshot: HashMap<String, String>,
    /// variable names whose value changed compared to previous entry
    pub changed_vars: Vec<String>,
    /// ["file.py:42 in fn()", …]
    pub stack_summary: Vec<String>,
}

/// Breakpoint spec with optional condition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BpSpec {
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// Result of evaluating one watch expression at a stop.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchResult {
    pub expression: String,
    pub value: String,
    pub changed: bool,
}

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
use crate::dap::client::{DapClient, encode_dap_frame};
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
    /// Breakpoints currently set per file: path → specs (line + optional condition).
    pub breakpoints: RwLock<HashMap<String, Vec<BpSpec>>>,
    /// Annotations added by the LLM (file:line → note).
    pub annotations: RwLock<Vec<Annotation>>,
    /// Findings / conclusions left by the LLM.
    pub findings: RwLock<Vec<Finding>>,
    /// Rolling buffer of output lines from the debuggee (last 500).
    pub console_lines: RwLock<std::collections::VecDeque<String>>,
    /// Increments on every `stopped` event — lets tools await the next pause.
    pub stopped_tx: Arc<tokio::sync::watch::Sender<u32>>,
    /// Last `stopped` event body — replayed to late-joining WS clients.
    pub last_stopped: Arc<RwLock<Option<Value>>>,
    annotation_counter: AtomicU32,
    finding_counter: AtomicU32,
    timeline_counter: AtomicU32,
    /// Rolling execution timeline — one entry per `stopped` event (capped at 500).
    pub timeline: RwLock<VecDeque<TimelineEntry>>,
    /// Watch expressions managed by MCP tools (evaluated automatically on every stop).
    pub watches: RwLock<Vec<String>>,
    /// Last evaluated results for each watch.
    pub watch_results: RwLock<Vec<WatchResult>>,
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

        // js-debug (NodeJs/TypeScript) spawns a TCP server and prints its port to stdout.
        // For those adapters, read the port and connect via TCP instead of using stdio.
        let mut js_debug_tcp_port: u16 = 0;
        let client = if adapter.is_tcp_after_spawn() {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let stdout = child.stdout.take().context("no stdout")?;
            let mut lines = BufReader::new(stdout).lines();
            let port: u16 = loop {
                let line = lines.next_line().await?.context("adapter closed before printing port")?;
                tracing::debug!("[{id}] js-debug stdout: {line}");
                // line looks like: "Debug server listening at ::1:12345"
                if let Some(p) = line.trim().rsplit(':').next().and_then(|s| s.parse().ok()) {
                    break p;
                }
            };
            tracing::info!("[{id}] js-debug TCP port: {port}");
            js_debug_tcp_port = port;
            // js-debug may bind on ::1 (IPv6) — try IPv6 first, fall back to IPv4
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let stream = match tokio::net::TcpStream::connect(format!("[::1]:{port}")).await {
                Ok(s) => s,
                Err(_) => tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await
                    .context("connecting to js-debug TCP server")?,
            };
            DapClient::from_tcp(stream, event_tx)
        } else {
            DapClient::new(&mut child, event_tx).context("creating DAP client")?
        };

        let (init_tx, init_rx) = tokio::sync::oneshot::channel::<()>();

        // Pre-register the session so broadcast() works before any WS client connects.
        hub.register(&id).await;

        let (stopped_tx, _) = tokio::sync::watch::channel(0u32);
        let stopped_tx = Arc::new(stopped_tx);
        let last_stopped_arc: Arc<RwLock<Option<Value>>> = Arc::new(RwLock::new(None));

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
            console_lines: RwLock::new(VecDeque::new()),
            stopped_tx: stopped_tx.clone(),
            last_stopped: last_stopped_arc.clone(),
            annotation_counter: AtomicU32::new(0),
            finding_counter: AtomicU32::new(0),
            timeline_counter: AtomicU32::new(0),
            timeline: RwLock::new(VecDeque::new()),
            watches: RwLock::new(Vec::new()),
            watch_results: RwLock::new(Vec::new()),
        });

        // Spawn event dispatcher: handles initialized signal + enriches stopped events
        {
            let hub2 = hub.clone();
            let client2 = client.clone();
            let session_id = id.clone();
            let stopped_tx2 = stopped_tx.clone();
            let last_stopped2 = last_stopped_arc.clone();
            let mut event_rx = event_rx;
            let mut init_tx_opt = Some(init_tx);
            // js-debug TCP port — needed to open child sessions for startDebugging
            // (non-zero only for NodeJs/TypeScript adapters)
            let js_debug_port: Option<u16> = if js_debug_tcp_port != 0 { Some(js_debug_tcp_port) } else { None };

            // Work-queue for startDebugging: any session at any depth pushes child configs
            // here; the supervisor task below spawns a handler for each one.  This avoids
            // recursive async fn which Rust can't prove Send.
            let (sd_tx, mut sd_rx) = mpsc::channel::<Value>(64);
            {
                let hub_s = hub.clone();
                let sid_s = id.clone();
                let stopped_s = stopped_tx.clone();
                let last_s = last_stopped_arc.clone();
                let sd_tx_s = sd_tx.clone();
                tokio::spawn(async move {
                    while let Some(cfg) = sd_rx.recv().await {
                        let h = hub_s.clone();
                        let s = sid_s.clone();
                        let st = stopped_s.clone();
                        let ls = last_s.clone();
                        let sd = sd_tx_s.clone();
                        if let Some(port) = js_debug_port {
                            tokio::spawn(async move {
                                if let Err(e) = attach_child_session(port, cfg, h, s, st, ls, sd).await {
                                    warn!("child session error: {e}");
                                }
                            });
                        }
                    }
                });
            }

            let session2 = session.clone();
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

                    // Handle reverse requests (e.g. startDebugging from js-debug)
                    if event.get("type").and_then(|v| v.as_str()) == Some("reverse_request_ack") {
                        let ack = event["ack"].clone();
                        let original = event["original"].clone();
                        let raw_ack = encode_dap_frame(&ack.to_string());
                        let _ = client2.send_raw(raw_ack).await;

                        // `startDebugging`: js-debug wants us to open a child DAP session.
                        if original.get("command").and_then(|v| v.as_str()) == Some("startDebugging") {
                            let pending_id = original["arguments"]["configuration"]["__pendingTargetId"]
                                .as_str().unwrap_or("").to_string();
                            let child_config = original["arguments"]["configuration"].clone();
                            if !pending_id.is_empty() {
                                let _ = sd_tx.send(child_config).await;
                            }
                        }
                        continue;
                    }

                    // Broadcast every other event to WebSocket clients
                    let ev_name = event.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                    info!("[{}] DAP event: {}", session_id, ev_name);
                    broadcast_json(&hub2, &session_id, event.clone()).await;

                    // Capture output events into the rolling console_lines buffer
                    if ev_name == "output" {
                        if let Some(output) = event.get("body").and_then(|b| b.get("output")).and_then(Value::as_str) {
                            let line = output.trim_end_matches('\n').to_string();
                            if !line.is_empty() {
                                let mut buf = session2.console_lines.write().await;
                                buf.push_back(line);
                                if buf.len() > 500 { buf.pop_front(); }
                            }
                        }
                    }

                    // On `stopped`: notify waiters + auto-chain data enrichment
                    if event.get("type").and_then(Value::as_str) == Some("event")
                        && event.get("event").and_then(Value::as_str) == Some("stopped")
                    {
                        // Store for late-joining WS clients
                        *last_stopped2.write().await = Some(event.clone());
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
                        enrich_stopped(&hub2, &client2, &session_id, Some(&session2), thread_id, &reason).await;
                    } else if event.get("event").and_then(Value::as_str) == Some("continued") {
                        *last_stopped2.write().await = None;
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
        let last_stopped_arc: Arc<RwLock<Option<Value>>> = Arc::new(RwLock::new(None));

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
            console_lines: RwLock::new(VecDeque::new()),
            stopped_tx: stopped_tx.clone(),
            last_stopped: last_stopped_arc.clone(),
            annotation_counter: AtomicU32::new(0),
            finding_counter: AtomicU32::new(0),
            timeline_counter: AtomicU32::new(0),
            timeline: RwLock::new(VecDeque::new()),
            watches: RwLock::new(Vec::new()),
            watch_results: RwLock::new(Vec::new()),
        });

        // Event dispatcher
        {
            let hub2 = hub.clone();
            let client2 = client.clone();
            let session_id = id.clone();
            let stopped_tx2 = stopped_tx.clone();
            let last_stopped2 = last_stopped_arc.clone();
            let mut event_rx = event_rx;
            let mut init_tx_opt = Some(init_tx);
            let session2 = session.clone();
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
                        *last_stopped2.write().await = Some(event.clone());
                        stopped_tx2.send_modify(|n| *n = n.wrapping_add(1));
                        let thread_id = event.get("body").and_then(|b| b.get("threadId"))
                            .and_then(Value::as_u64).unwrap_or(1) as u32;
                        let reason = event.get("body").and_then(|b| b.get("reason"))
                            .and_then(Value::as_str).unwrap_or("").to_string();
                        enrich_stopped(&hub2, &client2, &session_id, Some(&session2), thread_id, &reason).await;
                    } else if event.get("event").and_then(Value::as_str) == Some("continued") {
                        *last_stopped2.write().await = None;
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
    /// Returns immediately if the session is already paused.
    pub async fn wait_for_stop(&self, timeout_secs: u64) -> Result<()> {
        // If already paused, return immediately
        if self.last_stopped.read().await.is_some() {
            return Ok(());
        }
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
    /// Stores verified specs in `self.breakpoints`.
    pub async fn set_breakpoints(&self, file: &str, lines: Vec<u32>) -> Result<Value> {
        self.set_breakpoints_with_conditions(file, lines.into_iter().map(|l| BpSpec { line: l, condition: None }).collect()).await
    }

    /// Set breakpoints with optional conditions.
    pub async fn set_breakpoints_with_conditions(&self, file: &str, specs: Vec<BpSpec>) -> Result<Value> {
        let args = serde_json::json!({
            "source": { "path": file },
            "breakpoints": specs.iter().map(|s| {
                let mut obj = serde_json::json!({ "line": s.line });
                if let Some(cond) = &s.condition {
                    obj["condition"] = Value::String(cond.clone());
                }
                obj
            }).collect::<Vec<_>>()
        });
        let resp = self.client.request("setBreakpoints", Some(args)).await?;
        // Extract verified lines from the DAP response, preserve conditions from input specs
        let verified: Vec<BpSpec> = resp.get("body")
            .and_then(|b| b.get("breakpoints"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter()
                .filter_map(|b| {
                    let line = b.get("line").and_then(Value::as_u64).map(|l| l as u32)?;
                    let condition = specs.iter().find(|s| s.line == line).and_then(|s| s.condition.clone());
                    Some(BpSpec { line, condition })
                })
                .collect())
            .unwrap_or_else(|| specs.clone());
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

/// Handle js-debug's `startDebugging` reverse request by opening a second TCP
/// connection to the same js-debug server and sending `launch` with the configuration
/// forwarded from `startDebugging` (which includes `__pendingTargetId`).  All events from this child connection are routed to the same hub
/// session, so the UI sees stopped/stack/variable events as normal.
async fn attach_child_session(
    port: u16,
    child_config: Value,
    hub: Arc<Hub>,
    session_id: String,
    stopped_tx: Arc<tokio::sync::watch::Sender<u32>>,
    last_stopped: Arc<RwLock<Option<Value>>>,
    // Shared work-queue: push child configs here when startDebugging arrives.
    sd_tx: mpsc::Sender<Value>,
) -> Result<()> {
    let pending_id = child_config["__pendingTargetId"]
        .as_str().unwrap_or("?").to_string();
    // Small delay to let js-debug register the target
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    info!("[{session_id}] Opening child DAP session for target {pending_id}");

    let stream = match tokio::net::TcpStream::connect(format!("[::1]:{port}")).await {
        Ok(s) => s,
        Err(_) => tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await
            .context("child session TCP connect")?,
    };

    let (child_event_tx, mut child_event_rx) = mpsc::channel::<Value>(256);
    let child_client = DapClient::from_tcp(stream, child_event_tx);

    // 1. initialize
    let init_args = serde_json::json!({
        "clientID": "debugium-child",
        "adapterID": "pwa-node",
        "linesStartAt1": true,
        "columnsStartAt1": true,
        "pathFormat": "path",
    });
    child_client.request("initialize", Some(init_args)).await?;

    // 2. Wait for child `initialized` event
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, child_event_rx.recv()).await {
            Ok(Some(ev)) if ev.get("event").and_then(|v| v.as_str()) == Some("initialized") => break,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return Err(anyhow::anyhow!("child session closed before initialized")),
        }
    }

    // 3. launch the child session with the configuration forwarded from startDebugging
    child_client.notify("launch", Some(child_config.clone())).await
        .context("child launch")?;

    // Send setExceptionBreakpoints before configurationDone (mirrors what VS Code sends).
    child_client.notify("setExceptionBreakpoints", Some(serde_json::json!({
        "filters": []
    }))).await.context("child setExceptionBreakpoints")?;

    // configurationDone signals js-debug to activate the V8 debugger (Debugger.enable).
    child_client.notify("configurationDone", None).await
        .context("child configurationDone")?;

    info!("[{session_id}] Child session attached, waiting for events");

    // Forward events from the child session to the hub
    let hub2 = hub;
    let sid = session_id;
    tokio::spawn(async move {
        // js-debug sends a second `initialized` after connecting to the real V8 target.
        // We must respond with configurationDone so it activates the debugger (enables
        // `debugger` statement pausing).
        let mut initialized_count = 0u32;

        while let Some(event) = child_event_rx.recv().await {
            let ev_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if ev_type == "event" {
                let ev_name = event.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                info!("[{sid}] child DAP event: {ev_name}");

                if ev_name == "initialized" {
                    initialized_count += 1;
                    // The first `initialized` was consumed by the setup wait-loop above.
                    // Any `initialized` reaching this event loop is the real V8 target
                    // announcing it's ready — respond with configurationDone so js-debug
                    // activates Debugger.enable and honors `debugger` statements.
                    info!("[{sid}] child re-initialized (#{initialized_count}), sending configurationDone");
                    let _ = child_client.notify("configurationDone", None).await;
                } else if ev_name == "stopped" {
                    *last_stopped.write().await = Some(event.clone());
                    stopped_tx.send_modify(|n| *n = n.wrapping_add(1));
                    let thread_id = event.get("body").and_then(|b| b.get("threadId"))
                        .and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                    let reason = event.get("body").and_then(|b| b.get("reason"))
                        .and_then(|v| v.as_str()).unwrap_or("").to_string();
                    broadcast_json(&hub2, &sid, event.clone()).await;
                    enrich_stopped(&hub2, &child_client, &sid, None, thread_id, &reason).await;
                } else if ev_name == "continued" {
                    *last_stopped.write().await = None;
                    broadcast_json(&hub2, &sid, event.clone()).await;
                } else {
                    broadcast_json(&hub2, &sid, event).await;
                }
            } else if ev_type == "reverse_request_ack" {
                // DapClient wraps adapter reverse-requests as "reverse_request_ack" and
                // already sent the ack back.  Push startDebugging configs to the shared
                // work-queue so the supervisor opens the next-level child session.
                let original = &event["original"];
                let cmd = original.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if cmd == "startDebugging" {
                    let pending_id = original["arguments"]["configuration"]["__pendingTargetId"]
                        .as_str().unwrap_or("").to_string();
                    let grandchild_config = original["arguments"]["configuration"].clone();
                    if !pending_id.is_empty() {
                        info!("[{sid}] Queuing child session for target {pending_id}");
                        let _ = sd_tx.send(grandchild_config).await;
                    }
                }
            }
        }
        info!("[{sid}] child DAP session closed");
    });

    Ok(())
}

async fn broadcast_json(hub: &Hub, session_id: &str, msg: Value) {
    let envelope = WsEnvelope { session_id: session_id.to_string(), msg };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

/// On `stopped`: chain threads → stackTrace → scopes → variables, then push source content.
/// If `reason` is "exception", also call `exceptionInfo` and broadcast as a synthetic event.
/// If `session` is Some, also captures a timeline entry and evaluates watch expressions.
async fn enrich_stopped(
    hub: &Hub,
    client: &Arc<DapClient>,
    session_id: &str,
    session: Option<&Arc<Session>>,
    thread_id: u32,
    reason: &str,
) {
    // 1. threads
    let threads_resp = match client.request("threads", None).await {
        Ok(v) => { broadcast_json(hub, session_id, v.clone()).await; v }
        Err(e) => { warn!("threads failed: {e}"); return; }
    };
    let _ = threads_resp;

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

    // Stack summary for timeline
    let stack_summary: Vec<String> = frames.iter().map(|f| {
        let fname = f.get("name").and_then(Value::as_str).unwrap_or("?");
        let ffile = f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str)
            .map(|p| p.split('/').last().unwrap_or(p)).unwrap_or("?");
        let fline = f.get("line").and_then(Value::as_u64).unwrap_or(0);
        format!("{}:{} in {}()", ffile, fline, fname)
    }).collect();

    // 3. scopes
    let scopes_resp = match client.request("scopes", Some(serde_json::json!({ "frameId": frame_id }))).await {
        Ok(v) => { broadcast_json(hub, session_id, v.clone()).await; v }
        Err(e) => { warn!("scopes failed: {e}"); return; }
    };

    // 4. variables for each scope — collect locals for timeline
    let mut vars_snapshot: HashMap<String, String> = HashMap::new();
    if let Some(scopes) = scopes_resp.get("body").and_then(|b| b.get("scopes")).and_then(Value::as_array) {
        for (i, scope) in scopes.iter().enumerate() {
            let ref_ = scope.get("variablesReference").and_then(Value::as_u64).unwrap_or(0);
            if ref_ == 0 { continue; }
            let vars_args = serde_json::json!({ "variablesReference": ref_ });
            match client.request("variables", Some(vars_args)).await {
                Ok(v) => {
                    // Collect locals from the first (innermost) scope for the timeline
                    if i == 0 {
                        if let Some(vars) = v.get("body").and_then(|b| b.get("variables")).and_then(Value::as_array) {
                            for var in vars {
                                let name = var.get("name").and_then(Value::as_str).unwrap_or("?");
                                let val = var.get("value").and_then(Value::as_str).unwrap_or("?");
                                vars_snapshot.insert(name.to_string(), val.to_string());
                            }
                        }
                    }
                    broadcast_json(hub, session_id, v).await
                }
                Err(e) => warn!("variables({ref_}) failed: {e}"),
            }
        }
    }

    // 5. Timeline entry — only when session is provided (not for child sessions)
    if let Some(sess) = session {
        // Diff vars against the last timeline entry
        let prev_vars = sess.timeline.read().await
            .back()
            .map(|e| e.variables_snapshot.clone())
            .unwrap_or_default();
        let changed_vars: Vec<String> = vars_snapshot.iter()
            .filter(|(k, v)| prev_vars.get(*k).map(|pv| pv != *v).unwrap_or(true))
            .map(|(k, _)| k.clone())
            .collect();

        let entry_id = sess.timeline_counter.fetch_add(1, Ordering::SeqCst);
        let entry = TimelineEntry {
            id: entry_id,
            file: source_path.clone().unwrap_or_default(),
            line: source_line,
            timestamp: chrono::Utc::now().to_rfc3339(),
            variables_snapshot: vars_snapshot.clone(),
            changed_vars,
            stack_summary: stack_summary.clone(),
        };
        {
            let mut tl = sess.timeline.write().await;
            tl.push_back(entry.clone());
            if tl.len() > 500 { tl.pop_front(); }
        }
        // Broadcast timeline_entry
        let env = WsEnvelope {
            session_id: session_id.to_string(),
            msg: serde_json::json!({
                "type": "event",
                "event": "timeline_entry",
                "body": serde_json::to_value(&entry).unwrap_or(Value::Null)
            }),
        };
        if let Ok(json) = serde_json::to_string(&env) {
            hub.broadcast(session_id, json).await;
        }

        // 6. Evaluate watch expressions
        let watches = sess.watches.read().await.clone();
        if !watches.is_empty() {
            let prev_results = sess.watch_results.read().await.clone();
            let mut new_results: Vec<WatchResult> = Vec::new();
            for expr in &watches {
                let val = client.request("evaluate", Some(serde_json::json!({
                    "expression": expr,
                    "frameId": frame_id,
                    "context": "watch"
                }))).await
                    .ok()
                    .and_then(|r| r.get("body")?.get("result")?.as_str().map(str::to_string))
                    .unwrap_or_else(|| "<error>".to_string());
                let changed = prev_results.iter()
                    .find(|r| r.expression == *expr)
                    .map(|r| r.value != val)
                    .unwrap_or(true);
                new_results.push(WatchResult { expression: expr.clone(), value: val, changed });
            }
            *sess.watch_results.write().await = new_results.clone();
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: serde_json::json!({
                    "type": "event",
                    "event": "watches_updated",
                    "body": {
                        "results": serde_json::to_value(&new_results).unwrap_or(Value::Null)
                    }
                }),
            };
            if let Ok(json) = serde_json::to_string(&env) {
                hub.broadcast(session_id, json).await;
            }
        }
    }

    // 7. exceptionInfo — if stopped on an exception, fetch and broadcast details
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

    // 8. Read source file and push as synthetic `sourceLoaded` event
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
