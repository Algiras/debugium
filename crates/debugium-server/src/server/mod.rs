pub mod hub;
pub mod routes;

pub use routes::AppState;

use std::path::PathBuf;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::info;

use crate::dap::session::SessionRegistry;
use hub::Hub;

pub async fn start(
    hub: Arc<Hub>,
    sessions: Arc<SessionRegistry>,
    port: u16,
    static_dir: PathBuf,
    open_browser: bool,
) -> Result<()> {
    let state = AppState { hub, sessions };

    let app = Router::new()
        .route("/ws", get(routes::ws_handler))
        .route("/source", get(routes::source_handler))
        .route("/sessions", get(routes::sessions_handler))
        .nest_service("/", ServeDir::new(&static_dir))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Debugium listening on http://{addr}");

    if open_browser {
        let url = format!("http://localhost:{port}");
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
            let _ = open::that(url);
        });
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
