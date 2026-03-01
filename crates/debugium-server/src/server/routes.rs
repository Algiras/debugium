use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use dap_types::WsCommand;

use crate::dap::adapter::{Adapter, AdapterKind};
use crate::dap::session::{Session, SessionRegistry};
use crate::server::hub::Hub;

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    pub sessions: Arc<SessionRegistry>,
    pub session_counter: Arc<AtomicU32>,
}

// ─── WebSocket ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct WsParams {
    session: Option<String>,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<AppState>,
) -> Response {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    ws.on_upgrade(move |socket| handle_socket(socket, session_id, state))
}

async fn handle_socket(mut socket: WebSocket, session_id: String, state: AppState) {
    debug!("WS client connected for session [{session_id}]");

    let (mut rx, cache) = state.hub.subscribe(&session_id).await;

    // Send cached bootstrap messages
    for msg in cache {
        if socket.send(Message::Text(msg.into())).await.is_err() {
            return;
        }
    }


    loop {
        tokio::select! {
            // Incoming from adapter → forward to browser
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            // Incoming from browser → forward to session's DAP client
            Some(Ok(msg)) = socket.recv() => {
                match msg {
                    Message::Text(text) => {
                        if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                            handle_ui_command(cmd, &state).await;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            else => break,
        }
    }

    debug!("WS client disconnected for [{session_id}]");
}

async fn handle_ui_command(cmd: WsCommand, state: &AppState) {
    let Some(session) = state.sessions.get(&cmd.session_id).await else {
        warn!("unknown session: {}", cmd.session_id);
        return;
    };

    let args = if cmd.arguments.is_null() { None } else { Some(cmd.arguments) };

    // Commands that need their response broadcast back to the UI
    match cmd.command.as_str() {
        "evaluate" => {
            // Capture expression so we can inject it into the response (DAP responses don't echo it)
            let expr = args.as_ref().and_then(|a| a.get("expression")).cloned();
            match session.client.request(&cmd.command, args).await {
                Ok(mut resp) => {
                    if let (Some(expr_val), Some(body)) = (&expr, resp.get_mut("body")) {
                        if let Some(body_obj) = body.as_object_mut() {
                            body_obj.insert("expression".to_string(), expr_val.clone());
                        }
                    }
                    use dap_types::WsEnvelope;
                    let envelope = WsEnvelope { session_id: cmd.session_id.clone(), msg: resp };
                    if let Ok(json) = serde_json::to_string(&envelope) {
                        state.hub.broadcast(&cmd.session_id, json).await;
                    }
                }
                Err(e) => warn!("command evaluate failed: {e}"),
            }
        }
        "setBreakpoints" | "setExceptionBreakpoints" | "setVariable" | "completions"
        | "variables" | "scopes" | "stackTrace" => {
            match session.client.request(&cmd.command, args).await {
                Ok(resp) => {
                    use dap_types::WsEnvelope;
                    let envelope = WsEnvelope { session_id: cmd.session_id.clone(), msg: resp };
                    if let Ok(json) = serde_json::to_string(&envelope) {
                        state.hub.broadcast(&cmd.session_id, json).await;
                    }
                }
                Err(e) => warn!("command {} failed: {e}", cmd.command),
            }
        }
        _ => {
            if let Err(e) = session.client.notify(&cmd.command, args).await {
                warn!("command {} failed: {e}", cmd.command);
            }
        }
    }
}

// ─── Source file endpoint ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SourceParams {
    path: String,
}

pub async fn source_handler(
    Query(params): Query<SourceParams>,
) -> impl IntoResponse {
    match std::fs::read_to_string(&params.path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            Json(json!({ "lines": lines, "path": params.path })).into_response()
        }
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ─── Sessions list endpoint ───────────────────────────────────────────────────

pub async fn sessions_handler(State(state): State<AppState>) -> impl IntoResponse {
    let ids = state.sessions.list().await;
    let mut session_infos = Vec::new();
    for id in &ids {
        if let Some(session) = state.sessions.get(id).await {
            let meta = session.meta.read().await;
            let info = if let Some(m) = meta.as_ref() {
                let program_name = m.program.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                json!({
                    "id": id,
                    "status": "running",
                    "program": program_name,
                    "adapter": m.adapter_id,
                    "adapter_pid": m.adapter_pid,
                    "started_at": m.started_at.to_rfc3339(),
                    "port": m.port,
                })
            } else {
                json!({ "id": id, "status": "initializing" })
            };
            session_infos.push(info);
        } else {
            session_infos.push(json!({ "id": id }));
        }
    }
    Json(json!({ "sessions": session_infos }))
}

// ─── Launch new session endpoint ─────────────────────────────────────────────
//
// POST /sessions
// { "program": "/abs/path/to/file.py", "adapter": "python",
//   "breakpoints": ["file.py:42", "file.py:55"], "session_id": "optional-name" }

#[derive(Deserialize)]
pub struct LaunchRequest {
    pub program: String,
    pub adapter: Option<String>,
    pub breakpoints: Option<Vec<String>>,
    pub session_id: Option<String>,
}

pub async fn launch_session_handler(
    State(state): State<AppState>,
    Json(body): Json<LaunchRequest>,
) -> impl IntoResponse {
    let adapter_type = body.adapter.as_deref().unwrap_or("python");
    let kind = AdapterKind::from_str(adapter_type);
    let adapter = Adapter::new(kind);

    let session_id = body.session_id.unwrap_or_else(|| {
        let n = state.session_counter.fetch_add(1, Ordering::SeqCst);
        format!("session-{n}")
    });

    let session = match Session::new(session_id.clone(), adapter, state.hub.clone()).await {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    state.sessions.insert(session.clone()).await;

    let program = PathBuf::from(&body.program);
    let cwd = std::env::current_dir().unwrap_or_default();
    let breakpoints = parse_breakpoints_str(body.breakpoints.unwrap_or_default());

    tokio::spawn(async move {
        if let Err(e) = session.configure_and_launch(program, cwd, &breakpoints).await {
            tracing::error!("launch failed for session: {e}");
        }
    });

    Json(json!({ "session_id": session_id })).into_response()
}

// ─── Breakpoints endpoint ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SessionParam {
    session: Option<String>,
}

pub async fn breakpoints_handler(
    Query(params): Query<SessionParam>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    let Some(session) = state.sessions.get(&session_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "session not found" }))).into_response();
    };
    let bps = session.breakpoints.read().await.clone();
    Json(json!({ "breakpoints": bps })).into_response()
}

// ─── Annotations endpoint ─────────────────────────────────────────────────────

pub async fn annotations_handler(
    Query(params): Query<SessionParam>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    let Some(session) = state.sessions.get(&session_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "session not found" }))).into_response();
    };
    let anns = session.annotations.read().await.clone();
    let items: Vec<Value> = anns.iter().map(|a| json!({
        "id": a.id,
        "file": a.file,
        "line": a.line,
        "message": a.message,
        "color": a.color,
    })).collect();
    Json(json!({ "annotations": items })).into_response()
}

// ─── Findings endpoint ────────────────────────────────────────────────────────

pub async fn findings_handler(
    Query(params): Query<SessionParam>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    let Some(session) = state.sessions.get(&session_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "session not found" }))).into_response();
    };
    let finds = session.findings.read().await.clone();
    let items: Vec<Value> = finds.iter().map(|f| json!({
        "id": f.id,
        "message": f.message,
        "level": f.level,
        "timestamp": f.timestamp,
    })).collect();
    Json(json!({ "findings": items })).into_response()
}

/// Returns the last stopped event for the session, so late-joining UI clients
/// can restore the paused state without waiting for a new event.
pub async fn state_handler(
    Query(params): Query<SessionParam>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    let Some(session) = state.sessions.get(&session_id).await else {
        return Json(json!({ "paused": false })).into_response();
    };
    let last = session.last_stopped.read().await.clone();
    match last {
        Some(ev) => Json(json!({ "paused": true, "stopped_event": ev })).into_response(),
        None => Json(json!({ "paused": false })).into_response(),
    }
}

// ─── Timeline endpoint ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TimelineParams {
    session: Option<String>,
    limit: Option<usize>,
}

pub async fn timeline_handler(
    Query(params): Query<TimelineParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    let Some(session) = state.sessions.get(&session_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "session not found" }))).into_response();
    };
    let limit = params.limit.unwrap_or(50);
    let tl = session.timeline.read().await;
    let entries: Vec<_> = tl.iter().rev().take(limit).cloned().collect();
    let entries: Vec<_> = entries.into_iter().rev().collect();
    Json(json!({ "timeline": entries })).into_response()
}

// ─── Watches endpoint ─────────────────────────────────────────────────────────

pub async fn watches_handler(
    Query(params): Query<SessionParam>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let session_id = params.session.unwrap_or_else(|| "default".to_string());
    let Some(session) = state.sessions.get(&session_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "session not found" }))).into_response();
    };
    let watches = session.watches.read().await.clone();
    let results = session.watch_results.read().await.clone();
    Json(json!({ "watches": watches, "results": results })).into_response()
}

fn parse_breakpoints_str(raw: Vec<String>) -> Vec<(String, Vec<u32>)> {
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
