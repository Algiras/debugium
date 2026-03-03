//! MCP tool definitions, dispatch, and broadcast helpers.
//!
//! Extracted from `mod.rs` to keep the protocol/server code separate from
//! the tool implementations.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use crate::dap::session::SessionRegistry;
use crate::server::hub::Hub;

// ─── Broadcast breakpoints_changed so the UI can update gutter dots ──────────

async fn broadcast_breakpoints_changed(hub: &Arc<Hub>, session_id: &str, file: &str, specs: &[crate::dap::session::BpSpec]) {
    use dap_types::WsEnvelope;
    let lines: Vec<u32> = specs.iter().map(|s| s.line).collect();
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "breakpoints_changed",
            "body": { "file": file, "breakpoints": lines, "specs": specs }
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

// ─── Broadcast a synthetic llmQuery event so the UI shows LLM read access ────

async fn broadcast_llm_query(hub: &Arc<Hub>, session_id: &str, tool: &str, detail: &str) {
    use dap_types::WsEnvelope;
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "llmQuery",
            "body": { "tool": tool, "detail": detail }
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

    // Broadcast every tool call to the UI so the human sees AI activity in real time.
    // Individual tools may broadcast additional detail (matched output, etc).
    broadcast_llm_query(hub, session_id, name, "").await;

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
            let verified = s.breakpoints.read().await
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
                broadcast_breakpoints_changed(hub, session_id, file, &[] as &[crate::dap::session::BpSpec]).await;
            }
            Ok(format!("Cleared breakpoints in {} file(s): {:?}", files.len(), files))
        }

        // ── Execution control ──────────────────────────────────────────
        "continue_execution" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            broadcast_command(hub, session_id, "continue").await;
            // Capture line count BEFORE continuing so the LLM can pass it to wait_for_output.
            let console_line_count = s.console_lines.read().await.len();
            s.client.request("continue", Some(json!({ "threadId": thread_id }))).await?;
            Ok(serde_json::to_string_pretty(&json!({
                "status": "running",
                "console_line_count": console_line_count,
                "hint": "Pass console_line_count as from_line to wait_for_output to only match new output."
            }))?)
        }

        "step_over" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "next").await;
            let mut stopped_rx = s.stopped_tx.subscribe();
            s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
            let loc = await_stopped(&s, &mut stopped_rx).await;
            Ok(format!("Stepped over → {loc}"))
        }

        "step_in" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "stepIn").await;
            let mut stopped_rx = s.stopped_tx.subscribe();
            s.client.request("stepIn", Some(json!({ "threadId": thread_id }))).await?;
            let loc = await_stopped(&s, &mut stopped_rx).await;
            Ok(format!("Stepped in → {loc}"))
        }

        "step_out" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "stepOut").await;
            let mut stopped_rx = s.stopped_tx.subscribe();
            s.client.request("stepOut", Some(json!({ "threadId": thread_id }))).await?;
            let loc = await_stopped(&s, &mut stopped_rx).await;
            Ok(format!("Stepped out → {loc}"))
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
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };
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
                let tid = paused_thread_id(&s).await;
                let st = s.client.request("stackTrace", Some(json!({
                    "threadId": tid, "startFrame": 0, "levels": 1
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

        "set_data_breakpoint" => {
            use crate::dap::session::DataBpSpec;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let name = args.get("name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`name` is required"))?.to_string();
            let vref = args.get("variables_reference").and_then(Value::as_u64).unwrap_or(0);
            let access_type = args.get("access_type").and_then(Value::as_str).unwrap_or("write").to_string();
            let condition = args.get("condition").and_then(Value::as_str).map(str::to_string);
            let hit_condition = args.get("hit_condition").and_then(Value::as_str).map(str::to_string);

            // Check adapter capability first
            let caps = s.get_capabilities().await;
            let supports_data_bp = caps.get("supportsDataBreakpoints")
                .and_then(Value::as_bool).unwrap_or(false);
            if !supports_data_bp {
                return Ok(format!("Adapter does not support data breakpoints (supportsDataBreakpoints=false). Use step_until_change as an alternative."));
            }

            // 1. Query adapter for data breakpoint info (with timeout)
            let info_result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                s.client.request("dataBreakpointInfo", Some(json!({
                    "variablesReference": vref,
                    "name": name
                })))
            ).await;
            let info_resp = match info_result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return Ok(format!("Cannot set data breakpoint on '{name}': adapter error: {e}")),
                Err(_) => return Ok(format!("Cannot set data breakpoint on '{name}': adapter timed out")),
            };
            let body = info_resp.get("body").cloned().unwrap_or(json!(null));
            let data_id = body.get("dataId").and_then(Value::as_str).map(str::to_string);

            if let Some(data_id) = data_id {
                let label = body.get("description").and_then(Value::as_str)
                    .unwrap_or(&name).to_string();
                let spec = DataBpSpec {
                    data_id: data_id.clone(),
                    access_type: access_type.clone(),
                    label: label.clone(),
                    condition: condition.clone(),
                    hit_condition: hit_condition.clone(),
                };

                // Add to stored data breakpoints
                let mut dbps = s.data_breakpoints.write().await;
                dbps.retain(|d| d.data_id != data_id);
                dbps.push(spec);

                // 2. Set all data breakpoints with the adapter
                let bp_args: Vec<Value> = dbps.iter().map(|d| {
                    let mut obj = json!({ "dataId": d.data_id, "accessType": d.access_type });
                    if let Some(c) = &d.condition { obj["condition"] = json!(c); }
                    if let Some(hc) = &d.hit_condition { obj["hitCondition"] = json!(hc); }
                    obj
                }).collect();
                drop(dbps);

                let resp = s.client.request("setDataBreakpoints", Some(json!({
                    "breakpoints": bp_args
                }))).await?;

                let verified = resp.get("body").and_then(|b| b.get("breakpoints"))
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().any(|bp| bp.get("verified").and_then(Value::as_bool).unwrap_or(false)))
                    .unwrap_or(false);

                Ok(format!("Data breakpoint set on '{label}' (access: {access_type}, verified: {verified})"))
            } else {
                let reason = body.get("description").and_then(Value::as_str).unwrap_or("not eligible");
                Ok(format!("Cannot set data breakpoint on '{name}': {reason}"))
            }
        }

        "list_data_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let dbps = s.data_breakpoints.read().await.clone();
            Ok(serde_json::to_string_pretty(&json!({ "data_breakpoints": dbps }))?)
        }

        "clear_data_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let count = s.data_breakpoints.read().await.len();
            s.data_breakpoints.write().await.clear();
            s.client.request("setDataBreakpoints", Some(json!({ "breakpoints": [] }))).await?;
            Ok(format!("Cleared {count} data breakpoint(s)."))
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
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };
            let compact = args.get("verbosity").and_then(Value::as_str).unwrap_or("full") == "compact";
            let max_frames: usize = if compact { 3 } else { 20 };
            let max_vars: usize = if compact { 10 } else { 30 };
            let expand_depth = args.get("expand_depth").and_then(Value::as_u64).unwrap_or(1) as usize;

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

            // 2. scopes → locals with auto-expansion
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
                                        let child_ref = v.get("variablesReference").and_then(Value::as_u64).unwrap_or(0);

                                        if expand_depth > 0 && child_ref > 0 {
                                            let mut obj = serde_json::Map::new();
                                            if !typ.is_empty() { obj.insert("__type".into(), json!(typ)); }
                                            obj.insert("__value".into(), json!(val));
                                            if let Ok(child_resp) = s.client.request("variables",
                                                Some(json!({ "variablesReference": child_ref }))).await {
                                                if let Some(children) = child_resp.get("body")
                                                    .and_then(|b| b.get("variables")).and_then(Value::as_array) {
                                                    let max_children = 20;
                                                    for cv in children.iter().take(max_children) {
                                                        let cn = cv.get("name").and_then(Value::as_str).unwrap_or("?");
                                                        let cval = cv.get("value").and_then(Value::as_str).unwrap_or("?");
                                                        let ctyp = cv.get("type").and_then(Value::as_str).unwrap_or("");
                                                        let centry = if ctyp.is_empty() { json!(cval) }
                                                                     else { json!(format!("{cval} ({ctyp})")) };
                                                        obj.insert(cn.to_string(), centry);
                                                    }
                                                    if children.len() > max_children {
                                                        obj.insert("__truncated".into(), json!(format!("{} more", children.len() - max_children)));
                                                    }
                                                }
                                            }
                                            locals.insert(name.to_string(), json!(obj));
                                        } else {
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

            // 4. watches
            let watches: Vec<Value> = s.watch_results.read().await.iter().map(|wr| {
                json!({ "expression": wr.expression, "value": wr.value, "changed": wr.changed })
            }).collect();

            // 5. timeline delta — what changed since the last stop
            let changed_since_last_stop = {
                let tl = s.timeline.read().await;
                tl.back().map(|entry| {
                    json!({
                        "stop_id": entry.id,
                        "changed_vars": entry.changed_vars,
                    })
                }).unwrap_or(json!(null))
            };

            let bps = s.breakpoints.read().await.clone();
            let mut result = json!({
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
            if !watches.is_empty() {
                result["watches"] = json!(watches);
            }
            if !changed_since_last_stop.is_null() {
                result["changed_since_last_stop"] = changed_since_last_stop;
            }
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

        "continue_until" => {
            use crate::dap::session::BpSpec;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let target_line = require_u32(&args, "line")?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(15);

            // Save existing breakpoints for this file
            let original_specs: Vec<BpSpec> = s.breakpoints.read().await
                .get(file).cloned().unwrap_or_default();

            // Add the target line as a temporary breakpoint
            let mut with_temp = original_specs.clone();
            let already_has = with_temp.iter().any(|bp| bp.line == target_line);
            if !already_has {
                with_temp.push(BpSpec { line: target_line, condition: None, hit_condition: None, log_message: None });
            }
            s.set_breakpoints_with_conditions(file, with_temp).await?;

            // Continue execution
            s.client.request("continue", Some(json!({ "threadId": thread_id }))).await?;

            // Wait for stop
            let stopped = s.wait_for_stop(timeout).await;

            // Restore original breakpoints (remove temp) if we added one
            if !already_has {
                s.set_breakpoints_with_conditions(file, original_specs.clone()).await.ok();
                broadcast_breakpoints_changed(hub, session_id, file, &original_specs).await;
            }

            match stopped {
                Ok(_) => {
                    // Build a compact context summary inline
                    let stack_resp = s.client.request("stackTrace",
                        Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 3 }))).await.ok();
                    let top = stack_resp.as_ref()
                        .and_then(|r| r.get("body")).and_then(|b| b.get("stackFrames"))
                        .and_then(Value::as_array).and_then(|a| a.first());
                    let actual_file = top.and_then(|f| f.get("source")).and_then(|s| s.get("path"))
                        .and_then(Value::as_str).unwrap_or("?");
                    let actual_line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0);
                    let func_name = top.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("?");
                    let short = actual_file.split('/').last().unwrap_or(actual_file);
                    Ok(format!("Stopped at {short}:{actual_line} in {func_name}(). Call get_debug_context for full state."))
                }
                Err(_) => {
                    Ok(format!("Timed out after {timeout}s waiting to reach {file}:{target_line}. The program may still be running."))
                }
            }
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

        "explain_exception" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };

            // 1. Exception info
            let exc_info = s.client.request("exceptionInfo",
                Some(json!({ "threadId": thread_id }))).await
                .ok().and_then(|r| r.get("body").cloned()).unwrap_or(json!(null));

            // 2. Stack trace
            let stack_resp = s.client.request("stackTrace",
                Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 20 }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();
            let top = frames.first();
            let frame_id = top.and_then(|f| f.get("id")).and_then(Value::as_u64).unwrap_or(1) as u32;
            let file = top.and_then(|f| f.get("source")).and_then(|s| s.get("path"))
                .and_then(Value::as_str).unwrap_or("?").to_string();
            let line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let func = top.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("?").to_string();
            let call_stack: Vec<String> = frames.iter().map(|f| {
                format!("{}:{} in {}()",
                    f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str)
                        .map(|p| p.split('/').last().unwrap_or(p)).unwrap_or("?"),
                    f.get("line").and_then(Value::as_u64).unwrap_or(0),
                    f.get("name").and_then(Value::as_str).unwrap_or("?"))
            }).collect();

            // 3. Locals
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
                                    for v in vars.iter().take(30) {
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

            // 4. Recent console output (last 20 lines)
            let recent_output: Vec<String> = {
                let buf = s.console_lines.read().await;
                buf.iter().rev().take(20).cloned().collect::<Vec<_>>().into_iter().rev().collect()
            };

            // 5. Source window ±10 lines
            let source_window = if let Ok(content) = tokio::fs::read_to_string(&file).await {
                let lines: Vec<&str> = content.lines().collect();
                let ctx = 10usize;
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

            // 6. Recent timeline (last 5 stops)
            let recent_timeline: Vec<Value> = {
                let tl = s.timeline.read().await;
                tl.iter().rev().take(5).map(|entry| {
                    json!({
                        "stop_id": entry.id,
                        "file": entry.file.split('/').last().unwrap_or(&entry.file),
                        "line": entry.line,
                        "changed_vars": entry.changed_vars,
                    })
                }).collect::<Vec<_>>().into_iter().rev().collect()
            };

            Ok(serde_json::to_string_pretty(&json!({
                "exception": exc_info,
                "paused_at": format!("{}:{} in {}()",
                    file.split('/').last().unwrap_or(&file), line, func),
                "file": file,
                "line": line,
                "function": func,
                "frame_id": frame_id,
                "thread_id": thread_id,
                "locals": locals,
                "call_stack": call_stack,
                "source_window": source_window,
                "recent_output": recent_output,
                "recent_timeline": recent_timeline,
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

        "set_breakpoint" | "set_logpoint" => {
            use crate::dap::session::BpSpec;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let line = require_u32(&args, "line")?;
            let condition = args.get("condition").and_then(Value::as_str).map(str::to_string);
            let hit_condition = args.get("hit_condition").and_then(Value::as_str).map(str::to_string);
            let log_message = args.get("log_message")
                .or_else(|| args.get("message"))
                .and_then(Value::as_str).map(str::to_string);

            let new_spec = BpSpec { line, condition: condition.clone(), hit_condition: hit_condition.clone(), log_message: log_message.clone() };

            let mut existing: Vec<BpSpec> = s.breakpoints.read().await
                .get(file).cloned().unwrap_or_default();
            if let Some(pos) = existing.iter().position(|s| s.line == line) {
                existing[pos] = new_spec;
            } else {
                existing.push(new_spec);
            }
            let resp = s.set_breakpoints_with_conditions(file, existing).await?;
            let verified = resp.get("body").and_then(|b| b.get("breakpoints"))
                .and_then(Value::as_array)
                .and_then(|arr| arr.iter().find(|bp| {
                    bp.get("line").and_then(Value::as_u64).unwrap_or(0) == line as u64
                }))
                .and_then(|bp| bp.get("line").and_then(Value::as_u64))
                .unwrap_or(line as u64) as u32;
            let stored = s.breakpoints.read().await.get(file).cloned().unwrap_or_default();
            broadcast_breakpoints_changed(hub, session_id, file, &stored).await;

            let mut extras = Vec::new();
            if let Some(c) = &condition { extras.push(format!("condition: {c}")); }
            if let Some(hc) = &hit_condition { extras.push(format!("hit_condition: {hc}")); }
            if let Some(lm) = &log_message { extras.push(format!("log_message: {lm}")); }
            let suffix = if extras.is_empty() { String::new() } else { format!(" [{}]", extras.join(", ")) };
            let kind = if log_message.is_some() { "Logpoint" } else { "Breakpoint" };
            Ok(format!("{kind} set at {file}:{line} (verified line: {verified}){suffix}"))
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

        // ── Timeline ───────────────────────────────────────────────
        "get_timeline" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            let tl = s.timeline.read().await;
            let entries: Vec<_> = tl.iter().rev().take(limit).cloned().collect();
            let entries: Vec<_> = entries.into_iter().rev().collect();
            Ok(serde_json::to_string_pretty(&json!({ "timeline": entries, "total": tl.len() }))?)
        }

        // ── Watch expressions ──────────────────────────────────────
        "add_watch" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let expr = args.get("expression").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`expression` is required"))?;
            {
                let mut watches = s.watches.write().await;
                if !watches.contains(&expr.to_string()) {
                    watches.push(expr.to_string());
                }
            }
            // Broadcast watches_updated so the UI shows it immediately
            use dap_types::WsEnvelope;
            let watches_now = s.watches.read().await.clone();
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event",
                    "event": "watches_list_changed",
                    "body": { "watches": watches_now }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("Watch added: {expr}"))
        }

        "remove_watch" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let expr = args.get("expression").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`expression` is required"))?;
            s.watches.write().await.retain(|e| e != expr);
            s.watch_results.write().await.retain(|r| r.expression != expr);
            use dap_types::WsEnvelope;
            let watches_now = s.watches.read().await.clone();
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event",
                    "event": "watches_list_changed",
                    "body": { "watches": watches_now }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("Watch removed: {expr}"))
        }

        "get_watches" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let watches = s.watches.read().await.clone();
            let results = s.watch_results.read().await.clone();
            Ok(serde_json::to_string_pretty(&json!({
                "watches": watches,
                "results": results
            }))?)
        }

        "get_annotations" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let anns = s.annotations.read().await.clone();
            let detail = format!("{} annotation{}", anns.len(), if anns.len() == 1 { "" } else { "s" });
            broadcast_llm_query(hub, session_id, "get_annotations", &detail).await;
            Ok(serde_json::to_string_pretty(&json!({ "annotations": anns }))?)
        }

        "get_findings" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let findings = s.findings.read().await.clone();
            let detail = format!("{} finding{}", findings.len(), if findings.len() == 1 { "" } else { "s" });
            broadcast_llm_query(hub, session_id, "get_findings", &detail).await;
            Ok(serde_json::to_string_pretty(&json!({ "findings": findings }))?)
        }

        "get_variable_history" => {
            let name = args["name"].as_str()
                .ok_or_else(|| anyhow::anyhow!("get_variable_history: 'name' required"))?;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            broadcast_llm_query(hub, session_id, "get_variable_history", name).await;
            let tl = s.timeline.read().await;
            let history: Vec<Value> = tl.iter()
                .filter_map(|e| {
                    e.variables_snapshot.get(name).map(|val| json!({
                        "timeline_id": e.id,
                        "file": e.file,
                        "line": e.line,
                        "timestamp": e.timestamp,
                        "value": val
                    }))
                })
                .collect();
            let result_detail = if history.is_empty() {
                format!("{name} → no history")
            } else {
                format!("{name} → {} stop{}", history.len(), if history.len() == 1 { "" } else { "s" })
            };
            broadcast_llm_query(hub, session_id, "get_variable_history", &result_detail).await;
            Ok(serde_json::to_string_pretty(&json!({ "name": name, "history": history }))?)
        }

        "wait_for_output" => {
            let pattern = args["pattern"].as_str()
                .ok_or_else(|| anyhow::anyhow!("wait_for_output: 'pattern' required"))?;
            let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(10);
            let re = regex::Regex::new(pattern)
                .map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            // from_line: skip the first N lines of the buffer — use the console_line_count from
            // continue_execution to avoid matching output printed before the current run.
            let from_line = args.get("from_line").and_then(Value::as_u64).unwrap_or(0) as usize;
            broadcast_llm_query(hub, session_id, "wait_for_output", pattern).await;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
            loop {
                {
                    let lines = s.console_lines.read().await;
                    if let Some(line) = lines.iter().skip(from_line).find(|l| re.is_match(l)) {
                        let matched_line = line.clone();
                        drop(lines);
                        let detail = format!("matched: \"{}\"", matched_line.chars().take(60).collect::<String>());
                        broadcast_llm_query(hub, session_id, "wait_for_output", &detail).await;
                        return Ok(serde_json::to_string_pretty(&json!({ "matched": true, "line": matched_line }))?);
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    broadcast_llm_query(hub, session_id, "wait_for_output", "timed out").await;
                    return Ok(serde_json::to_string_pretty(&json!({ "matched": false, "line": Value::Null }))?);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        // ── Session lifecycle ─────────────────────────────────────
        "launch_session" => {
            let program_str = args.get("program").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`program` is required"))?;
            let program = std::path::PathBuf::from(program_str);
            let adapter_str = args.get("adapter").and_then(Value::as_str);
            let config_path = args.get("config").and_then(Value::as_str);

            let kind = if let Some(cfg) = config_path {
                crate::dap::adapter::AdapterKind::from_str(cfg)
            } else if let Some(a) = adapter_str {
                crate::dap::adapter::AdapterKind::from_str(a)
            } else {
                // Auto-detect: try multi-config, then extension, then default to python
                crate::dap::adapter::adapter_kind_from_extension(&program)
                    .unwrap_or(crate::dap::adapter::AdapterKind::Python)
            };

            let adapter = crate::dap::adapter::Adapter::new(kind);
            let sid = args.get("session_id").and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!("session-{}", chrono::Utc::now().timestamp_millis())
                });

            let session = crate::dap::session::Session::new(sid.clone(), adapter, hub.clone())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create session: {e}"))?;

            registry.insert(session.clone()).await;

            // Auto-remove session from registry after termination
            {
                let reg = registry.clone();
                let sid_cleanup = sid.clone();
                let mut term_rx = session.terminated_tx.subscribe();
                tokio::spawn(async move {
                    while term_rx.changed().await.is_ok() {
                        if *term_rx.borrow() {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            reg.remove(&sid_cleanup).await;
                            break;
                        }
                    }
                });
            }

            // Parse breakpoints
            let bp_strs: Vec<String> = args.get("breakpoints")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let breakpoints = parse_breakpoint_strings(&bp_strs);

            let cwd = std::env::current_dir().unwrap_or_default();

            // Run configure_and_launch and wait for it to complete (includes DAP handshake)
            let session2 = session.clone();
            let program2 = program.clone();
            let cwd2 = cwd.clone();
            let launch_handle = tokio::spawn(async move {
                session2.configure_and_launch(program2, cwd2, &breakpoints).await
            });

            // Wait up to 30s for the launch to complete
            match tokio::time::timeout(std::time::Duration::from_secs(30), launch_handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => return Err(anyhow::anyhow!("Launch failed: {e}")),
                Ok(Err(e)) => return Err(anyhow::anyhow!("Launch task panicked: {e}")),
                Err(_) => return Err(anyhow::anyhow!("Launch timed out after 30s")),
            }

            // Try to wait briefly for a breakpoint hit
            let status = match session.wait_for_stop(5).await {
                Ok(()) => "paused",
                Err(_) => "running",
            };

            // Broadcast session_launched on ALL existing sessions so connected UI clients hear it
            {
                use dap_types::WsEnvelope;
                let all_ids = registry.list().await;
                for existing_id in &all_ids {
                    let envelope = WsEnvelope {
                        session_id: sid.clone(),
                        msg: json!({
                            "type": "event",
                            "event": "session_launched",
                            "body": { "session_id": sid, "program": program_str, "status": status }
                        }),
                    };
                    if let Ok(j) = serde_json::to_string(&envelope) {
                        hub.broadcast(existing_id, j).await;
                    }
                }
            }

            Ok(serde_json::to_string_pretty(&json!({
                "session_id": sid,
                "status": status,
                "program": program_str,
                "hint": "Call get_debug_context to see where execution paused."
            }))?)
        }

        "stop_session" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;

            // Send disconnect to adapter
            let _ = s.client.request("disconnect", Some(json!({ "terminateDebuggee": true }))).await;

            // Kill adapter process if we have a PID
            if let Some(meta) = s.meta.read().await.as_ref() {
                if let Some(pid) = meta.adapter_pid {
                    let _ = nix_kill(pid);
                }
            }

            // Remove from registry
            registry.remove(session_id).await;

            Ok(format!("Session '{session_id}' stopped and removed."))
        }

        // ── New compound tools ─────────────────────────────────────
        "compare_snapshots" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let stop_a = args.get("stop_a").and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("`stop_a` is required"))? as u32;
            let stop_b = args.get("stop_b").and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("`stop_b` is required"))? as u32;
            broadcast_llm_query(hub, session_id, "compare_snapshots",
                &format!("stop {} vs {}", stop_a, stop_b)).await;
            let tl = s.timeline.read().await;
            let entry_a = tl.iter().find(|e| e.id == stop_a)
                .ok_or_else(|| anyhow::anyhow!("Timeline stop {} not found", stop_a))?;
            let entry_b = tl.iter().find(|e| e.id == stop_b)
                .ok_or_else(|| anyhow::anyhow!("Timeline stop {} not found", stop_b))?;
            let snap_a = &entry_a.variables_snapshot;
            let snap_b = &entry_b.variables_snapshot;
            let mut added = serde_json::Map::new();
            let mut removed = serde_json::Map::new();
            let mut changed = serde_json::Map::new();
            let mut unchanged_count: usize = 0;
            for (k, v_b) in snap_b {
                match snap_a.get(k) {
                    None => { added.insert(k.clone(), json!(v_b)); }
                    Some(v_a) if v_a != v_b => {
                        changed.insert(k.clone(), json!({ "from": v_a, "to": v_b }));
                    }
                    _ => { unchanged_count += 1; }
                }
            }
            for k in snap_a.keys() {
                if !snap_b.contains_key(k) {
                    removed.insert(k.clone(), json!(snap_a[k]));
                }
            }
            Ok(serde_json::to_string_pretty(&json!({
                "stop_a": { "id": entry_a.id, "file": entry_a.file, "line": entry_a.line, "timestamp": entry_a.timestamp },
                "stop_b": { "id": entry_b.id, "file": entry_b.file, "line": entry_b.line, "timestamp": entry_b.timestamp },
                "added": added,
                "removed": removed,
                "changed": changed,
                "unchanged_count": unchanged_count
            }))?)
        }

        "find_first_change" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let var_name = args.get("variable_name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`variable_name` is required"))?;
            let expected = args.get("expected_value").and_then(Value::as_str).map(str::to_string);
            broadcast_llm_query(hub, session_id, "find_first_change", var_name).await;
            let tl = s.timeline.read().await;
            // Collect entries oldest-first (timeline is stored newest-last already)
            let entries: Vec<_> = tl.iter().collect();
            let baseline = if let Some(ref exp) = expected {
                exp.clone()
            } else {
                // Use the initial observed value from the first entry that has this variable
                entries.iter()
                    .find_map(|e| e.variables_snapshot.get(var_name).cloned())
                    .unwrap_or_default()
            };
            let total = entries.len();
            let first_change = entries.iter().enumerate().find_map(|(i, e)| {
                let val = e.variables_snapshot.get(var_name)?;
                // Skip the very first entry when no expected_value: that's our baseline
                if expected.is_none() && i == 0 { return None; }
                if val != &baseline {
                    let old = if i > 0 {
                        entries[i - 1].variables_snapshot.get(var_name).cloned()
                    } else {
                        None
                    };
                    Some(json!({
                        "stop_id": e.id,
                        "file": e.file,
                        "line": e.line,
                        "timestamp": e.timestamp,
                        "old": old.as_deref().unwrap_or(&baseline),
                        "new": val
                    }))
                } else {
                    None
                }
            });
            Ok(serde_json::to_string_pretty(&json!({
                "variable": var_name,
                "baseline_value": baseline,
                "first_change_at": first_change,
                "total_stops_searched": total
            }))?)
        }

        "get_call_tree" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(5) as usize;
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };
            broadcast_command(hub, session_id, "stackTrace").await;
            let stack_resp = s.client.request("stackTrace", Some(json!({
                "threadId": thread_id, "startFrame": 0, "levels": max_depth
            }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();

            let mut call_tree: Vec<Value> = Vec::new();
            for (depth, frame) in frames.iter().take(max_depth).enumerate() {
                let frame_id = frame.get("id").and_then(Value::as_u64).unwrap_or(1) as u32;
                let file = frame.get("source").and_then(|s| s.get("path"))
                    .and_then(Value::as_str).unwrap_or("?").to_string();
                let line = frame.get("line").and_then(Value::as_u64).unwrap_or(0);
                let func = frame.get("name").and_then(Value::as_str).unwrap_or("?").to_string();

                // Fetch locals for this frame
                let mut locals = serde_json::Map::new();
                if let Ok(scopes_resp) = s.client.request("scopes",
                    Some(json!({ "frameId": frame_id }))).await {
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
                                    if let Some(vars) = vars_resp.get("body")
                                        .and_then(|b| b.get("variables"))
                                        .and_then(Value::as_array) {
                                        for v in vars.iter().take(20) {
                                            let vname = v.get("name").and_then(Value::as_str).unwrap_or("?");
                                            let val   = v.get("value").and_then(Value::as_str).unwrap_or("?");
                                            let typ   = v.get("type").and_then(Value::as_str).unwrap_or("");
                                            let entry = if typ.is_empty() { val.to_string() }
                                                        else { format!("{val} ({typ})") };
                                            locals.insert(vname.to_string(), json!(entry));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                call_tree.push(json!({
                    "depth": depth,
                    "file": file,
                    "line": line,
                    "function": func,
                    "frame_id": frame_id,
                    "locals": locals
                }));
            }
            Ok(serde_json::to_string_pretty(&call_tree)?)
        }

        "step_until_change" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let var_name = args.get("variable_name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`variable_name` is required"))?;
            let max_steps = args.get("max_steps").and_then(Value::as_u64).unwrap_or(20) as usize;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;

            // Get the current value via evaluate
            let get_value = |s: &Arc<crate::dap::session::Session>, tid: u32, expr: &str| {
                let s = s.clone();
                let expr = expr.to_string();
                async move {
                    let st = s.client.request("stackTrace",
                        Some(json!({ "threadId": tid, "startFrame": 0, "levels": 1 }))).await;
                    let frame_id = st.ok()
                        .and_then(|r| r.get("body")?.get("stackFrames")?.as_array()?.first().cloned())
                        .and_then(|f| f.get("id")?.as_u64())
                        .unwrap_or(1) as u32;
                    s.client.request("evaluate", Some(json!({
                        "expression": expr,
                        "frameId": frame_id,
                        "context": "watch"
                    }))).await
                        .ok()
                        .and_then(|r| r.get("body")?.get("result")?.as_str().map(str::to_string))
                        .unwrap_or_else(|| "<error>".to_string())
                }
            };

            let initial_value = get_value(&s, thread_id, var_name).await;
            broadcast_command(hub, session_id, &format!("step_until_change:{var_name}")).await;

            let mut steps = 0usize;
            let mut final_value = initial_value.clone();
            let mut changed = false;
            while steps < max_steps {
                let mut stopped_rx = s.stopped_tx.subscribe();
                s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
                await_stopped(&s, &mut stopped_rx).await;
                steps += 1;

                final_value = get_value(&s, thread_id, var_name).await;
                if final_value != initial_value {
                    changed = true;
                    break;
                }
            }

            // Get current location for context
            let loc = s.last_stopped.read().await;
            let paused_at = loc.as_ref()
                .and_then(|ev| ev.get("body"))
                .map(|_| {
                    // Read from stack in a best-effort way
                    json!(null)
                });
            drop(loc);

            // Get actual location via stack trace (best-effort)
            let paused_info = s.client.request("stackTrace",
                Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }))).await
                .ok()
                .and_then(|r| r.get("body")?.get("stackFrames")?.as_array()?.first().cloned())
                .map(|f| json!({
                    "file": f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str).unwrap_or("?"),
                    "line": f.get("line").and_then(Value::as_u64).unwrap_or(0),
                    "function": f.get("name").and_then(Value::as_str).unwrap_or("?")
                }))
                .unwrap_or(paused_at.unwrap_or(json!(null)));

            Ok(serde_json::to_string_pretty(&json!({
                "changed": changed,
                "variable": var_name,
                "from": initial_value,
                "to": final_value,
                "steps_taken": steps,
                "paused_at": paused_info
            }))?)
        }

        _ => Err(anyhow::anyhow!("Unknown tool: {name}")),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_breakpoint_strings(raw: &[String]) -> Vec<(String, Vec<u32>)> {
    let mut map: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
    for bp in raw {
        if let Some((file, line_str)) = bp.rsplit_once(':') {
            if let Ok(line) = line_str.parse::<u32>() {
                map.entry(file.to_string()).or_default().push(line);
            }
        }
    }
    map.into_iter().collect()
}

/// Kill a process by PID (best-effort).
fn nix_kill(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On Windows, use taskkill
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn require_u32(args: &Value, key: &str) -> Result<u32> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .ok_or_else(|| anyhow::anyhow!("`{key}` is required and must be an integer"))
}

/// Wait for the next stopped event (up to 15 s) and return a human-readable status string.
/// Subscribe BEFORE sending the DAP command to guarantee the event is never missed.
async fn await_stopped(
    session: &Arc<crate::dap::session::Session>,
    rx: &mut tokio::sync::watch::Receiver<u32>,
) -> String {
    match tokio::time::timeout(std::time::Duration::from_secs(15), rx.changed()).await {
        Err(_) => return "timed out waiting for stop".to_string(),
        Ok(_) => {}
    }
    let guard = session.last_stopped.read().await;
    guard.as_ref()
        .and_then(|ev| ev.get("body"))
        .map(|b| {
            let reason = b.get("reason").and_then(Value::as_str).unwrap_or("step");
            let thread = b.get("threadId").and_then(Value::as_u64).unwrap_or(1);
            format!("paused (reason={reason}, thread={thread}). Call get_debug_context for location.")
        })
        .unwrap_or_else(|| "paused. Call get_debug_context for location.".to_string())
}

/// Returns the threadId of the most recently stopped thread, falling back to 1.
async fn paused_thread_id(session: &Arc<crate::dap::session::Session>) -> u32 {
    let guard = session.last_stopped.read().await;
    guard
        .as_ref()
        .and_then(|ev: &Value| ev.get("body"))
        .and_then(|b: &Value| b.get("threadId"))
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32
}
