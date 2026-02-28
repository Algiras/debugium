use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};
use tracing::debug;

const CHANNEL_CAP: usize = 1024;

/// Per-session broadcast hub. Every WebSocket client subscribed to a session
/// receives every raw JSON frame that the DAP adapter sends.
pub struct Hub {
    sessions: RwLock<HashMap<String, broadcast::Sender<String>>>,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Get or create a broadcast channel for a session.
    pub async fn subscribe(&self, session_id: &str) -> broadcast::Receiver<String> {
        let mut sessions = self.sessions.write().await;
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(CHANNEL_CAP);
                tx
            })
            .subscribe()
    }

    /// Broadcast a JSON frame to all subscribers of a session.
    pub async fn broadcast(&self, session_id: &str, msg: String) {
        let sessions = self.sessions.read().await;
        if let Some(tx) = sessions.get(session_id) {
            debug!("[{session_id}] broadcast {}", &msg[..msg.len().min(120)]);
            // lagged receivers are dropped silently
            let _ = tx.send(msg);
        }
    }

    /// List all active session IDs.
    pub async fn session_ids(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }
}
