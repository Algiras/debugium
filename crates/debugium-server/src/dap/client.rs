use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, warn};

use dap_types::DapMessage;

type PendingMap = Arc<RwLock<HashMap<u32, oneshot::Sender<Value>>>>;

/// Async DAP client communicating over a subprocess's stdin/stdout
/// using Content-Length framing as per the DAP spec.
pub struct DapClient {
    seq: AtomicU32,
    sender: mpsc::Sender<String>,
    pending: PendingMap,
    event_tx: mpsc::Sender<Value>,
}

impl DapClient {
    /// Spawn a new DAP client wrapping the given child process.
    /// `event_tx` receives all unsolicited events.
    pub fn new(child: &mut Child, event_tx: mpsc::Sender<Value>) -> Result<Arc<Self>> {
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        let (tx, rx) = mpsc::channel::<String>(256);
        let pending: PendingMap = Arc::new(RwLock::new(HashMap::new()));

        let client = Arc::new(DapClient {
            seq: AtomicU32::new(1),
            sender: tx,
            pending: pending.clone(),
            event_tx,
        });

        // Write task: drains the outgoing channel → adapter stdin
        tokio::spawn(write_loop(stdin, rx));

        // Read task: reads Content-Length frames from adapter stdout
        tokio::spawn(read_loop(stdout, pending, client.event_tx.clone()));

        Ok(client)
    }

    /// Send a DAP request and wait for the response.
    pub async fn request(&self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let (otx, orx) = oneshot::channel();

        self.pending.write().await.insert(seq, otx);

        let msg = serde_json::json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments.unwrap_or(Value::Null),
        });

        let raw = encode_frame(&msg.to_string());
        self.sender.send(raw).await.context("send failed")?;

        let response = orx.await.context("adapter closed before responding")?;
        Ok(response)
    }

    /// Fire-and-forget: send a request without waiting for response.
    pub async fn notify(&self, command: &str, arguments: Option<Value>) -> Result<()> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let msg = serde_json::json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments.unwrap_or(Value::Null),
        });
        let raw = encode_frame(&msg.to_string());
        self.sender.send(raw).await.context("send failed")?;
        Ok(())
    }
}

// ─── Content-Length framing ────────────────────────────────────────────────────

fn encode_frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

async fn write_loop(mut stdin: ChildStdin, mut rx: mpsc::Receiver<String>) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = stdin.write_all(msg.as_bytes()).await {
            error!("write_loop error: {e}");
            break;
        }
    }
}

async fn read_loop(stdout: ChildStdout, pending: PendingMap, event_tx: mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    let mut header = String::new();

    loop {
        header.clear();
        match reader.read_line(&mut header).await {
            Ok(0) => {
                debug!("adapter stdout closed");
                break;
            }
            Err(e) => {
                error!("read_loop header error: {e}");
                break;
            }
            Ok(_) => {}
        }

        // Parse Content-Length
        let trimmed = header.trim();
        if !trimmed.starts_with("Content-Length:") {
            continue;
        }
        let content_len: usize = match trimmed["Content-Length:".len()..].trim().parse() {
            Ok(n) => n,
            Err(e) => {
                warn!("bad Content-Length: {e}");
                continue;
            }
        };

        // Skip blank line
        let mut blank = String::new();
        if reader.read_line(&mut blank).await.is_err() {
            break;
        }

        // Read body
        let mut buf = vec![0u8; content_len];
        if let Err(e) = tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf).await {
            error!("read_loop body error: {e}");
            break;
        }

        let raw = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => {
                warn!("non-utf8 body: {e}");
                continue;
            }
        };

        let msg: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                warn!("json parse error: {e}: {raw}");
                continue;
            }
        };

        debug!("[DAP IN] {msg}");

        match msg.get("type").and_then(Value::as_str) {
            Some("response") => {
                let seq = msg.get("request_seq").and_then(Value::as_u64).unwrap_or(0) as u32;
                if let Some(tx) = pending.write().await.remove(&seq) {
                    let _ = tx.send(msg);
                }
            }
            Some("event") => {
                let _ = event_tx.send(msg).await;
            }
            _ => {
                // reverse requests etc — forward as events
                let _ = event_tx.send(msg).await;
            }
        }
    }
}
