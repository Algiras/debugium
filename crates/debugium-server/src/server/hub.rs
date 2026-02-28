use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, RwLock};
use tracing::debug;

const CHANNEL_CAP: usize = 1024;

/// Per-session broadcast hub. Every WebSocket client subscribed to a session
/// receives every raw JSON frame that the DAP adapter sends.
pub struct Hub {
    sessions: RwLock<HashMap<String, (broadcast::Sender<String>, Vec<String>)>>,
}


impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Pre-register a session so that broadcast() works before any client subscribes.
    pub async fn register(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        sessions.entry(session_id.to_string()).or_insert_with(|| {
            let (tx, _) = broadcast::channel(CHANNEL_CAP);
            (tx, Vec::new())
        });
    }

    /// Get or create a broadcast channel for a session, and return a receiver
    /// along with any cached "sticky" messages to bootstrap the client.
    pub async fn subscribe(&self, session_id: &str) -> (broadcast::Receiver<String>, Vec<String>) {
        let mut sessions = self.sessions.write().await;
        let entry = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(CHANNEL_CAP);
                (tx, Vec::new())
            });

        let rx = entry.0.subscribe();
        let cache = entry.1.clone();
        (rx, cache)
    }

    /// Broadcast a JSON frame and cache it. Drops silently if session not registered.
    pub async fn broadcast(&self, session_id: &str, msg: String) {
        {
            let mut sessions = self.sessions.write().await;
            if let Some((tx, cache)) = sessions.get_mut(session_id) {
                // Keep a small cache of the last messages to help new clients sync
                // In a real app we'd filter for "important" ones, but for now we'll just keep the last 20
                cache.push(msg.clone());
                if cache.len() > 30 { cache.remove(0); }

                debug!("[{session_id}] broadcast {}", &msg[..msg.len().min(120)]);
                let _ = tx.send(msg.clone());
            }
        }

        // Append to ~/.debugium/sessions/<id>/events.ndjson (best-effort, never fatal)
        let session_id = session_id.to_string();
        let ts = chrono::Utc::now().to_rfc3339();
        let line = format!("{{\"ts\":\"{}\",\"msg\":{}}}\n", ts, msg);
        tokio::spawn(async move {
            if let Ok(home) = crate::home::DebugiumHome::open() {
                if let Ok(session_dir) = home.ensure_session_dir(&session_id) {
                    let events_path = session_dir.join("events.ndjson");
                    if let Ok(mut file) = tokio::fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&events_path)
                        .await
                    {
                        file.write_all(line.as_bytes()).await.ok();
                    }
                }
            }
        });
    }

    /// List all active session IDs.
    pub async fn session_ids(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }
}

