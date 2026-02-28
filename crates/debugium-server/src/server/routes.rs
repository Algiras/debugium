use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use dap_types::WsCommand;

use crate::dap::session::SessionRegistry;
use crate::server::hub::Hub;

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    pub sessions: Arc<SessionRegistry>,
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

    let mut rx: broadcast::Receiver<String> = state.hub.subscribe(&session_id).await;

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
    if let Err(e) = session.client.notify(&cmd.command, args).await {
        warn!("command {} failed: {e}", cmd.command);
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
    Json(json!({ "sessions": ids }))
}
