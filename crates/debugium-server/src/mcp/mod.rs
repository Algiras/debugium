/// MCP (Model Context Protocol) stdio server.
///
/// Implements the MCP 2024-11-05 spec over stdin/stdout JSON-RPC 2.0.
/// Claude Code connects to this via `claude mcp add` and can then drive
/// any active Debugium debug session using the tools defined here.
///
/// ## Protocol flow
///   stdin  → MCP/JSON-RPC requests from the LLM host  
///   stdout → MCP/JSON-RPC responses / notifications
///   stderr → tracing logs (so as not to pollute the protocol stream)

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

use crate::dap::session::SessionRegistry;
use crate::server::hub::Hub;

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

// ─── Tool definitions ────────────────────────────────────────────────────────

fn tool_list() -> Value {
    json!({
        "tools": [
            {
                "name": "get_sessions",
                "description": "List all active debug sessions.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "set_breakpoints",
                "description": "Set breakpoints in a source file. Replaces all existing breakpoints in that file. The web UI updates immediately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID (from get_sessions). Defaults to 'default'." },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "lines": { "type": "array", "items": { "type": "integer" }, "description": "Line numbers to break on (1-indexed)." }
                    },
                    "required": ["file", "lines"]
                }
            },
            {
                "name": "list_breakpoints",
                "description": "List all currently set breakpoints for a session, grouped by file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "clear_breakpoints",
                "description": "Remove all breakpoints from all files in a session. The web UI updates immediately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "continue_execution",
                "description": "Resume execution until the next breakpoint or program end.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to continue. Omit to continue all threads." }
                    },
                    "required": []
                }
            },
            {
                "name": "step_over",
                "description": "Execute the next line, stepping over function calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "step_in",
                "description": "Step into the function call on the current line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "step_out",
                "description": "Step out of the current function, returning to the caller.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "pause",
                "description": "Pause execution of a running thread.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "get_threads",
                "description": "Get all threads in the current process.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "get_stack_trace",
                "description": "Get the call stack for a thread. Use when the session is paused.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" },
                        "depth": { "type": "integer", "description": "Max frames to return. Default 20." }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "get_scopes",
                "description": "Get variable scopes for a stack frame (Locals, Globals, etc.).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "frame_id": { "type": "integer", "description": "Frame ID from get_stack_trace." }
                    },
                    "required": ["frame_id"]
                }
            },
            {
                "name": "get_variables",
                "description": "Get variables from a scope or expand a structured variable.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "variables_reference": { "type": "integer", "description": "variablesReference from get_scopes or a variable with children." }
                    },
                    "required": ["variables_reference"]
                }
            },
            {
                "name": "evaluate",
                "description": "Evaluate an expression in the context of a stack frame. Returns the result value.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "expression": { "type": "string", "description": "Expression to evaluate (e.g. 'len(my_list)', 'counter.value')." },
                        "frame_id": { "type": "integer", "description": "Stack frame context. Required for local variable access." },
                        "context": { "type": "string", "enum": ["watch", "repl", "hover", "clipboard"], "description": "Evaluation context. Default: 'repl'." }
                    },
                    "required": ["expression", "frame_id"]
                }
            },
            {
                "name": "get_source",
                "description": "Get the source code of a file, optionally highlighting the current execution line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the source file." },
                        "around_line": { "type": "integer", "description": "If set, returns only the N lines around this line number." },
                        "context_lines": { "type": "integer", "description": "Number of lines of context above/below around_line. Default 10." }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "get_debug_context",
                "description": "Get a compact, LLM-optimized snapshot of the current debug state in one call: paused location, local variables, call stack summary, source window (±5 lines), and active breakpoints. Use this instead of separate get_stack_trace + get_scopes + get_variables calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to inspect. Default 1." },
                        "verbosity": { "type": "string", "enum": ["compact", "full"], "description": "compact = top 10 vars + 3 frames; full = top 30 vars + all frames. Default: full." }
                    },
                    "required": []
                }
            },
            {
                "name": "annotate",
                "description": "Pin a visible note to a specific line in the source editor. The annotation appears as a colored gutter marker that the user can see immediately. Use this to mark suspicious lines, document findings inline, or flag areas of interest.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number (1-indexed)." },
                        "message": { "type": "string", "description": "The annotation text shown on hover." },
                        "color": { "type": "string", "enum": ["warning", "error", "info", "success"], "description": "Gutter marker color. Default: warning." }
                    },
                    "required": ["file", "line", "message"]
                }
            },
            {
                "name": "add_finding",
                "description": "Add a structured finding to the Findings panel visible in the UI. Use this to record conclusions, root causes, hypotheses, or important discoveries during a debugging session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "message": { "type": "string", "description": "The finding text." },
                        "level": { "type": "string", "enum": ["info", "warning", "bug", "fixed"], "description": "Severity / type. Default: info." }
                    },
                    "required": ["message"]
                }
            },
            {
                "name": "step_until",
                "description": "Step over repeatedly until a Python/JS expression evaluates to truthy, or until max_steps is reached. Returns the debug context at the stopping point. Much more efficient than calling step_over + evaluate in a loop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "condition": { "type": "string", "description": "Expression to evaluate after each step (e.g. 'counter.value > 50', 'result != None')." },
                        "max_steps": { "type": "integer", "description": "Maximum steps before giving up. Default 20." },
                        "thread_id": { "type": "integer", "description": "Thread to step. Default 1." }
                    },
                    "required": ["condition"]
                }
            },
            {
                "name": "run_until_exception",
                "description": "Continue execution until an exception is raised, then return full debug context at the crash site. Sets 'raised' exception breakpoints, continues, and returns the exception info + locals in one call.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to watch. Default 1." },
                        "timeout_secs": { "type": "integer", "description": "Max seconds to wait. Default 30." }
                    },
                    "required": []
                }
            },
            {
                "name": "disconnect",
                "description": "Terminate the debug session and the target process.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "terminate_debuggee": { "type": "boolean", "description": "Also kill the target process. Default true." }
                    },
                    "required": []
                }
            },
            {
                "name": "terminate",
                "description": "Send a terminate request to gracefully end the debuggee process.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "restart",
                "description": "Restart the debug session (restarts the debuggee process).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "set_exception_breakpoints",
                "description": "Configure which exception types cause the debugger to pause.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "filters": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["raised", "uncaught", "userUnhandled"] },
                            "description": "Exception filter names. Common: 'uncaught', 'raised'."
                        }
                    },
                    "required": ["filters"]
                }
            },
            {
                "name": "get_capabilities",
                "description": "Get the adapter capabilities returned during initialize. Useful to check if features like function breakpoints, exception info, or completions are supported.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "get_exception_info",
                "description": "Get detailed information about the last exception that caused the debugger to stop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread ID (from get_threads). Required." }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "set_variable",
                "description": "Change the value of a variable while the debugger is paused.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "variables_reference": { "type": "integer", "description": "variablesReference of the scope/object that contains the variable." },
                        "name": { "type": "string", "description": "Variable name to set." },
                        "value": { "type": "string", "description": "New value as a string expression." }
                    },
                    "required": ["variables_reference", "name", "value"]
                }
            },
            {
                "name": "set_breakpoint",
                "description": "Set a single breakpoint at a specific file and line. Optionally specify a condition expression. Returns the verified line number from the adapter.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number to break on (1-indexed)." },
                        "condition": { "type": "string", "description": "Optional condition expression — only pause when this evaluates to true." }
                    },
                    "required": ["file", "line"]
                }
            },
            {
                "name": "get_console_output",
                "description": "Return recent stdout/stderr output from the debuggee process (last N lines). Useful for reading program output without UI access.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "lines": { "type": "integer", "description": "Maximum number of recent lines to return. Default 50." }
                    },
                    "required": []
                }
            },
            {
                "name": "list_sessions",
                "description": "List all active debug sessions with enriched metadata: program, adapter, status, port, and start time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "set_function_breakpoints",
                "description": "Set breakpoints on function names rather than source lines.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "names": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Function names to break on (e.g. 'MyClass.method', 'free_fn')."
                        }
                    },
                    "required": ["names"]
                }
            }
        ]
    })
}

// ─── MCP server entry point ───────────────────────────────────────────────────

pub async fn serve(registry: Arc<SessionRegistry>, hub: Arc<Hub>, proxy_port: Option<u16>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

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

        let (response, is_notification) = match serde_json::from_str::<RpcRequest>(trimmed) {
            Err(e) => (RpcResponse::err(None, -32700, format!("Parse error: {e}")), false),
            Ok(req) => {
                let notification = req.id.is_none()
                    && req.method.starts_with("notifications/");
                (handle_request(req, &registry, &hub, proxy_port).await, notification)
            }
        };

        // Notifications must not generate a response (JSON-RPC / MCP spec)
        if is_notification { continue; }

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        debug!("[MCP OUT] {out}");
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }

    Ok(())
}

// ─── Request router ───────────────────────────────────────────────────────────

async fn handle_request(req: RpcRequest, registry: &Arc<SessionRegistry>, hub: &Arc<Hub>, proxy_port: Option<u16>) -> RpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        // MCP lifecycle
        "initialize" => {
            if let Some(port) = proxy_port {
                eprintln!("[Debugium MCP] Proxy mode → http://127.0.0.1:{port}");
            }
            // Print active sessions to stderr so the LLM can see what's available
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
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "debugium-mcp", "version": "0.1.0" }
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

// ─── Broadcast breakpoints_changed so the UI can update gutter dots ──────────

async fn broadcast_breakpoints_changed(hub: &Arc<Hub>, session_id: &str, file: &str, lines: &[u32]) {
    use dap_types::WsEnvelope;
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "breakpoints_changed",
            "body": { "file": file, "breakpoints": lines }
        }),
    };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

// ─── Broadcast a synthetic commandSent event so the web UI can animate it ────

async fn broadcast_command(hub: &Arc<Hub>, session_id: &str, command: &str) {
    use dap_types::WsEnvelope;
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "commandSent",
            "body": { "command": command, "origin": "mcp" }
        }),
    };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

// ─── Tool dispatcher ─────────────────────────────────────────────────────────

pub async fn dispatch_tool(
    name: &str,
    args: Value,
    registry: &Arc<SessionRegistry>,
    hub: &Arc<Hub>,
) -> Result<String> {
    let session_id = args.get("session_id").and_then(Value::as_str).unwrap_or("default");
    let session = registry.get(session_id).await;

    match name {
        // ── Session ────────────────────────────────────────────────────
        "get_sessions" => {
            let ids = registry.list().await;
            Ok(serde_json::to_string_pretty(&json!({ "sessions": ids }))?)
        }

        // ── Breakpoints ────────────────────────────────────────────────
        "set_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let lines: Vec<u32> = args.get("lines").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("`lines` is required"))?
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect();
            let resp = s.set_breakpoints(file, lines.clone()).await?;
            // Broadcast breakpoints_changed so the UI updates immediately
            let verified: Vec<u32> = s.breakpoints.read().await
                .get(file).cloned().unwrap_or_default();
            broadcast_breakpoints_changed(hub, session_id, file, &verified).await;
            Ok(format!("Breakpoints set in {file} at lines {:?}\n{}", lines,
                serde_json::to_string_pretty(&resp)?))
        }

        "list_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let bps = s.breakpoints.read().await.clone();
            Ok(serde_json::to_string_pretty(&json!({ "breakpoints": bps }))?)
        }

        "clear_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let files: Vec<String> = s.breakpoints.read().await.keys().cloned().collect();
            for file in &files {
                s.set_breakpoints(file, vec![]).await?;
                broadcast_breakpoints_changed(hub, session_id, file, &[]).await;
            }
            Ok(format!("Cleared breakpoints in {} file(s): {:?}", files.len(), files))
        }

        // ── Execution control ──────────────────────────────────────────
        "continue_execution" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            broadcast_command(hub, session_id, "continue").await;
            let resp = s.client.request("continue", Some(json!({ "threadId": thread_id }))).await?;
            Ok(format!("Continued thread {thread_id}\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "step_over" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "next").await;
            let resp = s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
            Ok(format!("Step over on thread {thread_id}\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "step_in" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "stepIn").await;
            let resp = s.client.request("stepIn", Some(json!({ "threadId": thread_id }))).await?;
            Ok(format!("Step in on thread {thread_id}\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "step_out" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "stepOut").await;
            let resp = s.client.request("stepOut", Some(json!({ "threadId": thread_id }))).await?;
            Ok(format!("Step out on thread {thread_id}\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "pause" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "pause").await;
            let resp = s.client.request("pause", Some(json!({ "threadId": thread_id }))).await?;
            Ok(format!("Paused thread {thread_id}\n{}", serde_json::to_string_pretty(&resp)?))
        }

        // ── Inspection ─────────────────────────────────────────────────
        "get_threads" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            broadcast_command(hub, session_id, "threads").await;
            let resp = s.client.request("threads", None).await?;
            let threads = resp.get("body").and_then(|b| b.get("threads")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&threads)?)
        }

        "get_stack_trace" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(20);
            broadcast_command(hub, session_id, "stackTrace").await;
            let resp = s.client.request("stackTrace", Some(json!({
                "threadId": thread_id, "startFrame": 0, "levels": depth
            }))).await?;
            let frames = resp.get("body").and_then(|b| b.get("stackFrames")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&frames)?)
        }

        "get_scopes" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let frame_id = require_u32(&args, "frame_id")?;
            broadcast_command(hub, session_id, "scopes").await;
            let resp = s.client.request("scopes", Some(json!({ "frameId": frame_id }))).await?;
            let scopes = resp.get("body").and_then(|b| b.get("scopes")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&scopes)?)
        }

        "get_variables" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let vref = require_u32(&args, "variables_reference")?;
            broadcast_command(hub, session_id, "variables").await;
            let resp = s.client.request("variables", Some(json!({ "variablesReference": vref }))).await?;
            let vars = resp.get("body").and_then(|b| b.get("variables")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&vars)?)
        }

        "evaluate" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let expr = args.get("expression").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`expression` is required"))?;
            // frame_id is optional; auto-resolve top frame when omitted
            let frame_id = if let Some(v) = args.get("frame_id").and_then(Value::as_u64) {
                v as u32
            } else {
                let st = s.client.request("stackTrace", Some(json!({
                    "threadId": 1, "startFrame": 0, "levels": 1
                }))).await?;
                st.get("body").and_then(|b| b["stackFrames"].as_array())
                    .and_then(|a| a.first())
                    .and_then(|f| f["id"].as_u64())
                    .unwrap_or(1) as u32
            };
            let context = args.get("context").and_then(Value::as_str).unwrap_or("repl");
            let resp = s.client.request("evaluate", Some(json!({
                "expression": expr,
                "frameId": frame_id,
                "context": context
            }))).await?;
            let result = resp.get("body").and_then(|b| b.get("result")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&result)?)
        }

        // ── Source ─────────────────────────────────────────────────────
        "get_source" => {
            let path = args.get("path").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`path` is required"))?;
            let content = tokio::fs::read_to_string(path).await
                .map_err(|e| anyhow::anyhow!("Could not read {path}: {e}"))?;
            let lines: Vec<&str> = content.lines().collect();

            if let Some(center) = args.get("around_line").and_then(Value::as_u64).map(|n| n as usize) {
                let ctx = args.get("context_lines").and_then(Value::as_u64).unwrap_or(10) as usize;
                let start = center.saturating_sub(ctx + 1);
                let end = (center + ctx).min(lines.len());
                let snippet: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        let n = start + i + 1;
                        let marker = if n == center { "→" } else { " " };
                        format!("{marker} {n:4}: {l}")
                    })
                    .collect();
                Ok(snippet.join("\n"))
            } else {
                let numbered: Vec<String> = lines.iter().enumerate()
                    .map(|(i, l)| format!("{:4}: {l}", i + 1))
                    .collect();
                Ok(numbered.join("\n"))
            }
        }

        // ── Lifecycle ──────────────────────────────────────────────────
        "terminate" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let resp = s.client.request("terminate", Some(json!({ "restart": false }))).await?;
            Ok(format!("Terminate requested.\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "restart" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            s.client.notify("restart", None).await?;
            Ok("Restart requested.".to_string())
        }

        "set_exception_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let filters: Vec<String> = args.get("filters").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("`filters` is required"))?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            let resp = s.client.request("setExceptionBreakpoints", Some(json!({ "filters": filters }))).await?;
            Ok(format!("Exception breakpoints set: {:?}\n{}", filters, serde_json::to_string_pretty(&resp)?))
        }

        "get_capabilities" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let caps = s.get_capabilities().await;
            Ok(serde_json::to_string_pretty(&caps)?)
        }

        "get_exception_info" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            let resp = s.client.request("exceptionInfo", Some(json!({ "threadId": thread_id }))).await?;
            let body = resp.get("body").cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&body)?)
        }

        "set_variable" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let vref = require_u32(&args, "variables_reference")?;
            let name = args.get("name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`name` is required"))?;
            let value = args.get("value").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`value` is required"))?;
            let resp = s.client.request("setVariable", Some(json!({
                "variablesReference": vref,
                "name": name,
                "value": value
            }))).await?;
            Ok(format!("Variable '{name}' set to '{value}'\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "set_function_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let names: Vec<&str> = args.get("names").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("`names` is required"))?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let bps: Vec<Value> = names.iter().map(|n| json!({ "name": n })).collect();
            let resp = s.client.request("setFunctionBreakpoints", Some(json!({ "breakpoints": bps }))).await?;
            Ok(format!("Function breakpoints set: {:?}\n{}", names, serde_json::to_string_pretty(&resp)?))
        }

        // ── Compound / LLM-optimised tools ────────────────────────────
        "get_debug_context" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            let compact = args.get("verbosity").and_then(Value::as_str).unwrap_or("full") == "compact";
            let max_frames: usize = if compact { 3 } else { 20 };
            let max_vars: usize = if compact { 10 } else { 30 };

            // 1. stack trace
            let stack_resp = s.client.request("stackTrace", Some(json!({
                "threadId": thread_id, "startFrame": 0, "levels": max_frames
            }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();
            let top = frames.first();
            let frame_id = top.and_then(|f| f.get("id")).and_then(Value::as_u64).unwrap_or(1) as u32;
            let file = top.and_then(|f| f.get("source")).and_then(|s| s.get("path"))
                .and_then(Value::as_str).unwrap_or("?").to_string();
            let line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let func = top.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("?").to_string();
            let stack_summary: Vec<String> = frames.iter().map(|f| {
                let fname = f.get("name").and_then(Value::as_str).unwrap_or("?");
                let ffile = f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str)
                    .map(|p| p.split('/').last().unwrap_or(p)).unwrap_or("?");
                let fline = f.get("line").and_then(Value::as_u64).unwrap_or(0);
                format!("{}:{} in {}()", ffile, fline, fname)
            }).collect();

            // 2. scopes → locals
            let mut locals = serde_json::Map::new();
            if let Ok(scopes_resp) = s.client.request("scopes", Some(json!({ "frameId": frame_id }))).await {
                let scopes = scopes_resp.get("body").and_then(|b| b.get("scopes"))
                    .and_then(Value::as_array).cloned().unwrap_or_default();
                let locals_scope = scopes.iter().find(|sc| {
                    sc.get("name").and_then(Value::as_str)
                        .map(|n| n.to_lowercase().contains("local") || n.to_lowercase().contains("function"))
                        .unwrap_or(false)
                }).or_else(|| scopes.first());
                if let Some(sc) = locals_scope {
                    if let Some(vref) = sc.get("variablesReference").and_then(Value::as_u64) {
                        if vref > 0 {
                            if let Ok(vars_resp) = s.client.request("variables",
                                Some(json!({ "variablesReference": vref }))).await {
                                if let Some(vars) = vars_resp.get("body").and_then(|b| b.get("variables"))
                                    .and_then(Value::as_array) {
                                    for v in vars.iter().take(max_vars) {
                                        let name = v.get("name").and_then(Value::as_str).unwrap_or("?");
                                        let val  = v.get("value").and_then(Value::as_str).unwrap_or("?");
                                        let typ  = v.get("type").and_then(Value::as_str).unwrap_or("");
                                        let entry = if typ.is_empty() { val.to_string() }
                                                    else { format!("{val} ({typ})") };
                                        locals.insert(name.to_string(), json!(entry));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 3. source window ±5 lines
            let source_window = if let Ok(content) = tokio::fs::read_to_string(&file).await {
                let lines: Vec<&str> = content.lines().collect();
                let ctx = 5usize;
                let start = (line as usize).saturating_sub(ctx + 1);
                let end = ((line as usize) + ctx).min(lines.len());
                lines[start..end].iter().enumerate()
                    .map(|(i, l)| {
                        let n = start + i + 1;
                        let marker = if n == line as usize { "→" } else { " " };
                        format!("{marker} {:4}: {l}", n)
                    })
                    .collect::<Vec<_>>().join("\n")
            } else { String::new() };

            let bps = s.breakpoints.read().await.clone();
            let result = json!({
                "paused_at": format!("{}:{} in {}()",
                    file.split('/').last().unwrap_or(&file), line, func),
                "file": file,
                "line": line,
                "function": func,
                "frame_id": frame_id,
                "thread_id": thread_id,
                "locals": locals,
                "call_stack": stack_summary,
                "source_window": source_window,
                "breakpoints": bps,
            });
            Ok(serde_json::to_string_pretty(&result)?)
        }

        "annotate" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let line = require_u32(&args, "line")?;
            let message = args.get("message").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`message` is required"))?;
            let color = args.get("color").and_then(Value::as_str).unwrap_or("warning");
            let ann = s.add_annotation(file.to_string(), line, message.to_string(), color.to_string()).await;
            // Broadcast so the UI updates immediately
            use dap_types::WsEnvelope;
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event", "event": "annotation_added",
                    "body": { "id": ann.id, "file": ann.file, "line": ann.line,
                              "message": ann.message, "color": ann.color }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("[{}] {}:{} — {}", color, file.split('/').last().unwrap_or(file), line, message))
        }

        "add_finding" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let message = args.get("message").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`message` is required"))?;
            let level = args.get("level").and_then(Value::as_str).unwrap_or("info");
            let f = s.add_finding(message.to_string(), level.to_string()).await;
            use dap_types::WsEnvelope;
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event", "event": "finding_added",
                    "body": { "id": f.id, "message": f.message, "level": f.level, "timestamp": f.timestamp }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("[{}] {}", level, message))
        }

        "step_until" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let condition = args.get("condition").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`condition` is required"))?;
            let max_steps = args.get("max_steps").and_then(Value::as_u64).unwrap_or(20) as usize;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;

            let mut steps = 0;
            let mut hit = false;
            while steps < max_steps {
                s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
                s.wait_for_stop(15).await?;
                steps += 1;

                // Get top frame id for evaluate
                let frame_id = s.client.request("stackTrace",
                    Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }))).await
                    .ok()
                    .and_then(|r| r.get("body")?.get("stackFrames")?.as_array()?.first().cloned())
                    .and_then(|f| f.get("id")?.as_u64())
                    .unwrap_or(1) as u32;

                let eval = s.client.request("evaluate", Some(json!({
                    "expression": condition,
                    "frameId": frame_id,
                    "context": "watch"
                }))).await;
                let result = eval.ok()
                    .and_then(|r| r.get("body")?.get("result")?.as_str().map(str::to_string))
                    .unwrap_or_else(|| "False".to_string());
                if result != "False" && result != "false" && result != "0" && result != "None" {
                    hit = true;
                    break;
                }
            }
            let status = if hit { format!("Condition met after {steps} step(s)") }
                         else { format!("Reached max_steps={max_steps} without condition firing") };
            Ok(status)
        }

        "run_until_exception" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(30);

            // Enable 'raised' exception breakpoints
            if let Ok(caps) = s.get_capabilities().await.get("exceptionBreakpointFilters")
                .and_then(Value::as_array).map(|a| !a.is_empty()).ok_or(()) {
                if caps {
                    s.client.request("setExceptionBreakpoints",
                        Some(json!({ "filters": ["raised"] }))).await.ok();
                }
            } else {
                s.client.request("setExceptionBreakpoints",
                    Some(json!({ "filters": ["raised"] }))).await.ok();
            }

            // Wait briefly for any in-flight stop event (handles race where exception
            // fires just as this tool is called and last_stopped isn't set yet)
            if s.last_stopped.read().await.is_none() {
                // Give up to 5s for a pending stop event before deciding to continue
                let _ = s.wait_for_stop(5).await;
            }
            // If already stopped at an exception, use current state; otherwise continue and wait
            let already_at_exception = s.last_stopped.read().await
                .as_ref()
                .and_then(|ev| ev.get("body"))
                .and_then(|b| b.get("reason"))
                .and_then(Value::as_str)
                .map(|r| r == "exception")
                .unwrap_or(false);
            if !already_at_exception {
                s.client.request("continue", Some(json!({ "threadId": thread_id }))).await?;
                s.wait_for_stop(timeout).await?;
            }

            // Reuse get_debug_context logic: call recursively via a fake args
            let ctx_args = json!({ "session_id": session_id, "thread_id": thread_id });
            let ctx_name = "get_debug_context";
            // Inline: get stack + scopes + vars
            let stack_resp = s.client.request("stackTrace",
                Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 10 }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();
            let top = frames.first();
            let frame_id = top.and_then(|f| f.get("id")).and_then(Value::as_u64).unwrap_or(1) as u32;
            let file = top.and_then(|f| f.get("source")).and_then(|s| s.get("path"))
                .and_then(Value::as_str).unwrap_or("?").to_string();
            let line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let func = top.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("?").to_string();

            // Exception info
            let exc_info = s.client.request("exceptionInfo",
                Some(json!({ "threadId": thread_id }))).await
                .ok()
                .and_then(|r| r.get("body").cloned())
                .unwrap_or(json!(null));

            let _ = ctx_args; let _ = ctx_name;
            Ok(serde_json::to_string_pretty(&json!({
                "exception": exc_info,
                "paused_at": format!("{}:{} in {}()",
                    file.split('/').last().unwrap_or(&file), line, func),
                "file": file, "line": line, "function": func, "frame_id": frame_id,
                "call_stack": frames.iter().map(|f| {
                    format!("{}:{} in {}()",
                        f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str)
                            .map(|p| p.split('/').last().unwrap_or(p)).unwrap_or("?"),
                        f.get("line").and_then(Value::as_u64).unwrap_or(0),
                        f.get("name").and_then(Value::as_str).unwrap_or("?"))
                }).collect::<Vec<_>>(),
            }))?)
        }

        "disconnect" => {
            let terminate = args.get("terminate_debuggee").and_then(Value::as_bool).unwrap_or(true);
            if let Some(s) = session {
                let resp = s.client.request("disconnect", Some(json!({ "terminateDebuggee": terminate }))).await?;
                Ok(format!("Disconnected.\n{}", serde_json::to_string_pretty(&resp)?))
            } else {
                Ok("No active session to disconnect.".to_string())
            }
        }

        "set_breakpoint" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let line = require_u32(&args, "line")?;
            let condition = args.get("condition").and_then(Value::as_str).map(str::to_string);

            // Build breakpoints list preserving existing ones, add/replace this line
            let mut existing: Vec<u32> = s.breakpoints.read().await
                .get(file).cloned().unwrap_or_default();
            if !existing.contains(&line) {
                existing.push(line);
            }
            let bp_args = if let Some(cond) = &condition {
                let specs: Vec<Value> = existing.iter().map(|&l| {
                    if l == line { json!({ "line": l, "condition": cond }) }
                    else { json!({ "line": l }) }
                }).collect();
                json!({ "source": { "path": file }, "breakpoints": specs })
            } else {
                let specs: Vec<Value> = existing.iter().map(|&l| json!({ "line": l })).collect();
                json!({ "source": { "path": file }, "breakpoints": specs })
            };
            let resp = s.client.request("setBreakpoints", Some(bp_args)).await?;
            let verified = resp.get("body").and_then(|b| b.get("breakpoints"))
                .and_then(Value::as_array)
                .and_then(|arr| arr.iter().find(|bp| {
                    bp.get("line").and_then(Value::as_u64).unwrap_or(0) == line as u64
                }))
                .and_then(|bp| bp.get("line").and_then(Value::as_u64))
                .unwrap_or(line as u64) as u32;
            // Update stored breakpoints
            s.breakpoints.write().await.entry(file.to_string()).or_default().push(line);
            broadcast_breakpoints_changed(hub, session_id, file, &s.breakpoints.read().await.get(file).cloned().unwrap_or_default()).await;
            Ok(format!("Breakpoint set at {}:{} (verified line: {})", file, line, verified))
        }

        "get_console_output" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let n = args.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
            let buf = s.console_lines.read().await;
            let lines: Vec<&str> = buf.iter().rev().take(n).map(|s| s.as_str()).collect();
            let output: Vec<&str> = lines.into_iter().rev().collect();
            if output.is_empty() {
                Ok("(no output yet)".to_string())
            } else {
                Ok(output.join("\n"))
            }
        }

        "list_sessions" => {
            let ids = registry.list().await;
            let mut result = Vec::new();
            for sid in &ids {
                if let Some(s) = registry.get(sid).await {
                    let meta = s.meta.read().await;
                    let entry = if let Some(m) = meta.as_ref() {
                        json!({
                            "id": sid,
                            "program": m.program.display().to_string(),
                            "adapter": m.adapter_id,
                            "adapter_pid": m.adapter_pid,
                            "started_at": m.started_at,
                            "port": m.port,
                        })
                    } else {
                        json!({ "id": sid, "status": "initializing" })
                    };
                    result.push(entry);
                }
            }
            Ok(serde_json::to_string_pretty(&json!({ "sessions": result }))?)
        }

        _ => Err(anyhow::anyhow!("Unknown tool: {name}")),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn require_u32(args: &Value, key: &str) -> Result<u32> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .ok_or_else(|| anyhow::anyhow!("`{key}` is required and must be an integer"))
}
