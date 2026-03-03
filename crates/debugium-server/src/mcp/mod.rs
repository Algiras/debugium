/// MCP (Model Context Protocol) stdio server.
///
/// Implements MCP 2025-11-25 over stdin/stdout JSON-RPC 2.0.
/// Supports optional elicitation (form/url) when the connecting client
/// declares the capability — otherwise those code paths are no-ops.
///
/// ## Protocol flow
///   stdin  → MCP/JSON-RPC requests from the LLM host  
///   stdout → MCP/JSON-RPC responses / notifications / server-initiated requests
///   stderr → tracing logs (so as not to pollute the protocol stream)

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tracing::debug;

use crate::dap::session::SessionRegistry;
use crate::server::hub::Hub;

mod tools;
pub use tools::dispatch_tool;
use tools::tool_list;

// ─── Wire types ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcResponse {
    fn ok(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    fn err(id: Option<Value>, code: i32, msg: impl Into<String>) -> Self {
        Self { jsonrpc: "2.0", id, result: None, error: Some(RpcError { code, message: msg.into() }) }
    }
}


// ─── Client capability tracking ──────────────────────────────────────────────

#[derive(Default, Clone)]
struct ClientCapabilities {
    elicitation_form: bool,
    elicitation_url: bool,
}

impl ClientCapabilities {
    fn from_init_params(params: &Value) -> Self {
        let caps = params.get("capabilities").unwrap_or(&Value::Null);
        let elicit = caps.get("elicitation");
        let (mut form, mut url) = (false, false);
        if let Some(e) = elicit {
            if e.is_object() {
                // Be strict: only enable when the client explicitly declares support.
                form = e.get("form").is_some();
                url = e.get("url").is_some();
            }
        }
        Self { elicitation_form: form, elicitation_url: url }
    }
}

// ─── Server-to-client request channel (for elicitation) ─────────────────────

type PendingResponses = Arc<tokio::sync::Mutex<
    std::collections::HashMap<u64, oneshot::Sender<Value>>,
>>;

static NEXT_SERVER_REQ_ID: AtomicU64 = AtomicU64::new(1_000_000);

/// Shared context threaded through tool dispatch — holds capabilities +
/// the channel needed to send server→client requests (elicitation).
#[derive(Clone)]
struct McpContext {
    client_caps: Arc<tokio::sync::RwLock<ClientCapabilities>>,
    outbox: tokio::sync::mpsc::Sender<String>,
    pending: PendingResponses,
}

impl McpContext {
    /// Send an elicitation/form request to the client.
    /// Returns `None` immediately if the client did not declare `elicitation.form`.
    #[allow(dead_code)]
    async fn elicit_form(
        &self,
        message: &str,
        schema: Value,
    ) -> Option<Value> {
        if !self.client_caps.read().await.elicitation_form {
            return None;
        }
        let req_id = NEXT_SERVER_REQ_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id, tx);

        let req = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "elicitation/create",
            "params": {
                "message": message,
                "requestedSchema": schema,
            }
        });
        if let Ok(mut s) = serde_json::to_string(&req) {
            s.push('\n');
            let _ = self.outbox.send(s).await;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(v)) => Some(v),
            _ => {
                self.pending.lock().await.remove(&req_id);
                None
            }
        }
    }

    /// Send an elicitation/url request (open a URL in the client).
    /// Returns `None` if the client did not declare `elicitation.url`.
    #[allow(dead_code)]
    async fn elicit_url(
        &self,
        message: &str,
        url: &str,
    ) -> Option<Value> {
        if !self.client_caps.read().await.elicitation_url {
            return None;
        }
        let req_id = NEXT_SERVER_REQ_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id, tx);

        let req = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "elicitation/createUrl",
            "params": {
                "message": message,
                "url": url,
            }
        });
        if let Ok(mut s) = serde_json::to_string(&req) {
            s.push('\n');
            let _ = self.outbox.send(s).await;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(v)) => Some(v),
            _ => {
                self.pending.lock().await.remove(&req_id);
                None
            }
        }
    }
}

// ─── MCP server entry point ───────────────────────────────────────────────────

pub async fn serve(registry: Arc<SessionRegistry>, hub: Arc<Hub>, proxy_port: Option<u16>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let client_caps = Arc::new(tokio::sync::RwLock::new(ClientCapabilities::default()));
    let pending: PendingResponses = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let (outbox_tx, mut outbox_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Writer task: multiplexes normal responses + server-initiated requests onto stdout
    let write_handle = tokio::spawn(async move {
        while let Some(msg) = outbox_rx.recv().await {
            debug!("[MCP OUT async] {}", msg.trim());
            if stdout.write_all(msg.as_bytes()).await.is_err() { break; }
            if stdout.flush().await.is_err() { break; }
        }
    });

    let mcp_ctx = McpContext {
        client_caps: client_caps.clone(),
        outbox: outbox_tx.clone(),
        pending: pending.clone(),
    };

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF — host disconnected
        }

        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        debug!("[MCP IN] {trimmed}");

        // Incoming messages may be client→server requests OR responses to
        // our server→client requests (elicitation). Distinguish by presence
        // of "method" field: responses have "result"/"error" but no "method".
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let resp = RpcResponse::err(None, -32700, format!("Parse error: {e}"));
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                let _ = outbox_tx.send(out).await;
                continue;
            }
        };

        if parsed.get("method").is_none() {
            // This is a response to a server-initiated request (elicitation).
            if let Some(id) = parsed.get("id").and_then(Value::as_u64) {
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let result = parsed.get("result").cloned().unwrap_or(Value::Null);
                    let _ = tx.send(result);
                }
            }
            continue;
        }

        let req: RpcRequest = match serde_json::from_value(parsed) {
            Ok(r) => r,
            Err(e) => {
                let resp = RpcResponse::err(None, -32700, format!("Parse error: {e}"));
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                let _ = outbox_tx.send(out).await;
                continue;
            }
        };

        let notification = req.id.is_none() && req.method.starts_with("notifications/");
        let response = handle_request(req, &registry, &hub, proxy_port, &client_caps, &mcp_ctx).await;

        if notification { continue; }

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        debug!("[MCP OUT] {out}");
        let _ = outbox_tx.send(out).await;
    }

    // Ensure all sender clones are dropped so the writer task can exit cleanly.
    drop(mcp_ctx);
    drop(outbox_tx);
    let _ = write_handle.await;
    Ok(())
}

// ─── Request router ───────────────────────────────────────────────────────────

async fn handle_request(
    req: RpcRequest,
    registry: &Arc<SessionRegistry>,
    hub: &Arc<Hub>,
    proxy_port: Option<u16>,
    client_caps: &Arc<tokio::sync::RwLock<ClientCapabilities>>,
    _mcp_ctx: &McpContext,
) -> RpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        // MCP lifecycle
        "initialize" => {
            if let Some(params) = &req.params {
                *client_caps.write().await = ClientCapabilities::from_init_params(params);
            }
            let caps = client_caps.read().await;
            eprintln!("[Debugium MCP] Client elicitation: form={} url={}", caps.elicitation_form, caps.elicitation_url);
            drop(caps);

            if let Some(port) = proxy_port {
                eprintln!("[Debugium MCP] Proxy mode → http://127.0.0.1:{port}");
            }
            let ids = registry.list().await;
            if ids.is_empty() && proxy_port.is_none() {
                eprintln!("[Debugium MCP] No active sessions. Use POST /sessions or launch with --mcp to start one.");
            } else if !ids.is_empty() {
                eprintln!("[Debugium MCP] Active sessions:");
                for sid in &ids {
                    if let Some(s) = registry.get(sid).await {
                        let meta = s.meta.read().await;
                        if let Some(m) = meta.as_ref() {
                            eprintln!("  session_id={sid}  program={}  adapter={}", m.program.display(), m.adapter_id);
                        } else {
                            eprintln!("  session_id={sid}  (initializing)");
                        }
                    }
                }
            }
            RpcResponse::ok(id, json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "debugium-mcp", "version": "0.2.0" }
            }))
        }
        "notifications/initialized" => {
            return RpcResponse::ok(None, json!(null));
        }
        "ping" => RpcResponse::ok(id, json!({})),

        // Tool discovery
        "tools/list" => RpcResponse::ok(id, tool_list()),

        // Tool invocation
        "tools/call" => {
            let params = req.params.unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);

            // In proxy mode, always forward to the running server
            if let Some(port) = proxy_port {
                match proxy_tool_via_http(port, &name, args).await {
                    Ok(content) => return RpcResponse::ok(id, json!({ "content": [{ "type": "text", "text": content }] })),
                    Err(e) => return RpcResponse::err(id, -32603, e.to_string()),
                }
            }

            // Local dispatch (launch --mcp mode)
            match dispatch_tool(&name, args, registry, hub).await {
                Ok(content) => RpcResponse::ok(id, json!({ "content": [{ "type": "text", "text": content }] })),
                Err(e) => RpcResponse::err(id, -32603, e.to_string()),
            }
        }

        _ => RpcResponse::err(id, -32601, format!("Method not found: {}", req.method)),
    }
}

// ─── HTTP proxy for standalone MCP mode ──────────────────────────────────────

async fn proxy_tool_via_http(
    port: u16,
    tool: &str,
    args: Value,
) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/mcp-proxy"))
        .json(&json!({ "tool": tool, "args": args }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot reach Debugium server at port {port}: {e}"))?;
    let body: Value = resp.json().await
        .map_err(|e| anyhow::anyhow!("Invalid response from server: {e}"))?;
    if body["ok"].as_bool() == Some(true) {
        Ok(body["result"].as_str().unwrap_or("{}").to_string())
    } else {
        Err(anyhow::anyhow!("{}", body["error"].as_str().unwrap_or("proxy error")))
    }
}

