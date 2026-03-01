pub mod hub;
pub mod routes;

pub use routes::AppState;

use std::path::PathBuf;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::{get, post};
use axum::Router;
use std::sync::atomic::AtomicU32;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::info;

use crate::dap::session::SessionRegistry;
use hub::Hub;

/// Start the HTTP server. If `port` is 0 the OS picks a free port.
/// Sends the actual bound port on `port_tx` before entering the serve loop,
/// so callers can discover the port without waiting for the server to stop.
pub async fn start(
    hub: Arc<Hub>,
    sessions: Arc<SessionRegistry>,
    port: u16,
    static_dir: PathBuf,
    open_browser: bool,
) -> Result<u16> {
    let state = AppState {
        hub,
        sessions,
        session_counter: Arc::new(AtomicU32::new(1)),
    };

    let app = Router::new()
        .route("/ws", get(routes::ws_handler))
        .route("/source", get(routes::source_handler))
        .route("/sessions", get(routes::sessions_handler))
        .route("/sessions", post(routes::launch_session_handler))
        .route("/breakpoints", get(routes::breakpoints_handler))
        .route("/annotations", get(routes::annotations_handler))
        .route("/findings", get(routes::findings_handler))
        .route("/state", get(routes::state_handler))
        .route("/timeline", get(routes::timeline_handler))
        .route("/watches", get(routes::watches_handler))
        .route("/mcp-proxy", post(routes::mcp_proxy_handler))
        .fallback_service(ServeDir::new(&static_dir))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();
    info!("Debugium listening on http://localhost:{actual_port}");

    if open_browser {
        let url = format!("http://localhost:{actual_port}");
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
            let _ = open::that(url);
        });
    }

    axum::serve(listener, app).await?;
    Ok(actual_port)
}

/// Start server in background; resolves the port immediately via a oneshot channel.
pub async fn start_background(
    hub: Arc<Hub>,
    sessions: Arc<SessionRegistry>,
    port: u16,
    static_dir: PathBuf,
    open_browser: bool,
) -> Result<u16> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();

    let state = AppState {
        hub,
        sessions,
        session_counter: Arc::new(std::sync::atomic::AtomicU32::new(1)),
    };

    let app = axum::Router::new()
        .route("/ws", axum::routing::get(routes::ws_handler))
        .route("/source", axum::routing::get(routes::source_handler))
        .route("/sessions", axum::routing::get(routes::sessions_handler))
        .route("/sessions", axum::routing::post(routes::launch_session_handler))
        .route("/breakpoints", axum::routing::get(routes::breakpoints_handler))
        .route("/annotations", axum::routing::get(routes::annotations_handler))
        .route("/findings", axum::routing::get(routes::findings_handler))
        .route("/state", axum::routing::get(routes::state_handler))
        .route("/timeline", axum::routing::get(routes::timeline_handler))
        .route("/watches", axum::routing::get(routes::watches_handler))
        .route("/mcp-proxy", axum::routing::post(routes::mcp_proxy_handler))
        .fallback_service(tower_http::services::ServeDir::new(&static_dir))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    info!("Debugium listening on http://localhost:{actual_port}");

    if open_browser {
        let url = format!("http://localhost:{actual_port}");
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
            let _ = open::that(url);
        });
    }

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("Web server error: {e}");
        }
    });

    Ok(actual_port)
}
