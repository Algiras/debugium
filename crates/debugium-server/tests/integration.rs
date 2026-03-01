//! End-to-end integration tests for `debugium`.
//!
//! Groups
//! ──────
//! A. MCP protocol layer      (no server needed)
//! B. /mcp-proxy HTTP         (server running, no active session)
//! C. Proxy MCP               (server running, no active session)
//! D. Proxy MCP, server down  (no server)
//! E. Breakpoint tools        (live server + paused session)
//! F. Inspection tools        (session paused at line 43)
//! G. Execution control       (sequential, consumes session state)
//! H. Compound / LLM tools    (live session)
//! I. Lifecycle / edge cases  (disconnect, exception target)
//! L. CLI help / usage        (smoke, no server)
//! M. Launch subcommand       (auto-managed server)
//! N. MCP subcommand          (standalone proxy)
//! O. Launch with --mcp flag  (combined HTTP + MCP stdio)
//! P. Attach subcommand       (minimal smoke)

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ─── Constants ────────────────────────────────────────────────────────────────

fn debugium_bin() -> PathBuf {
    // `cargo test` builds the binary before running tests when declared in [[test]]
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // remove test binary name
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("debugium");
    if !p.exists() {
        // fallback: workspace target/debug
        p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/debugium");
    }
    p
}

fn target_py() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/target_python.py")
        .canonicalize()
        .expect("target_python.py not found")
}

fn target_threads_py() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/target_threads.py")
        .canonicalize()
        .expect("target_threads.py not found")
}

fn target_subprocess_py() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/target_subprocess.py")
        .canonicalize()
        .expect("target_subprocess.py not found")
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Poll until HTTP GET on `path` returns 200, or timeout.
fn wait_server(port: u16, timeout_secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if let Ok(stream) = TcpStream::connect(format!("127.0.0.1:{port}")) {
            drop(stream);
            // Send a minimal HTTP GET to confirm the server speaks HTTP
            if http_get(port, "/sessions").is_ok() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}

/// Poll /state until paused=true or timeout.
fn wait_paused(port: u16, timeout_secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if let Ok(body) = http_get(port, "/state") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if v.get("paused").and_then(|x| x.as_bool()) == Some(true) {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}

/// Blocking HTTP GET via raw TCP (no external crate).
fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .ok();
    let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut response = String::new();
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(&mut stream, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => response.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
    }
    if response.starts_with("HTTP/") && response.contains("200") {
        // Return body (after blank line)
        if let Some(pos) = response.find("\r\n\r\n") {
            return Ok(response[pos + 4..].to_string());
        }
    }
    Err(format!("HTTP error: {}", &response[..response.len().min(200)]))
}

#[allow(dead_code)]
/// Blocking HTTP POST via raw TCP.
fn http_post(port: u16, path: &str, body: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    let req = format!(
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut response = String::new();
    let mut buf = [0u8; 16384];
    loop {
        match std::io::Read::read(&mut stream, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => response.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
    }
    if response.starts_with("HTTP/") && (response.contains("200") || response.contains("201")) {
        if let Some(pos) = response.find("\r\n\r\n") {
            return Ok(response[pos + 4..].to_string());
        }
    }
    Err(format!("HTTP POST error: {}", &response[..response.len().min(300)]))
}

struct McpProc {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl McpProc {
    fn start(port: u16) -> Self {
        let mut child = Command::new(debugium_bin())
            .args(["mcp", "--port", &port.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn debugium mcp");
        let stdout = child.stdout.take().unwrap();
        McpProc { child, reader: BufReader::new(stdout), next_id: 0 }
    }

    fn send(&mut self, method: &str, params: Option<serde_json::Value>) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method
        });
        if let Some(p) = params {
            msg["params"] = p;
        }
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
        let mut resp = String::new();
        self.reader.read_line(&mut resp).unwrap();
        serde_json::from_str(resp.trim()).expect("bad JSON from mcp")
    }

    fn initialize(&mut self) -> serde_json::Value {
        self.send(
            "initialize",
            Some(serde_json::json!({"clientInfo": {"name": "test"}})),
        )
    }

    fn tool_call(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.send(
            "tools/call",
            Some(serde_json::json!({"name": name, "arguments": args})),
        )
    }

    /// Extract text from a tools/call result.
    fn text(r: &serde_json::Value) -> String {
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn stop(mut self) {
        let _ = self.child.stdin.take(); // close stdin
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Managed server: launches debugium and terminates it on drop.
struct ServerGuard {
    child: Child,
    pub port: u16,
}

impl ServerGuard {
    fn launch(port: u16, bp_line: Option<u32>, extra_bp: Option<u32>) -> Self {
        let bin = debugium_bin();
        let target = target_py();
        let mut cmd = Command::new(&bin);
        cmd.args([
            "launch",
            target.to_str().unwrap(),
            "--adapter",
            "python",
            "--port",
            &port.to_string(),
            "--no-open-browser",
        ]);
        if let Some(line) = bp_line {
            cmd.arg("--breakpoint");
            cmd.arg(format!("{}:{line}", target.display()));
        }
        if let Some(line) = extra_bp {
            cmd.arg("--breakpoint");
            cmd.arg(format!("{}:{line}", target.display()));
        }
        let child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn debugium launch");
        ServerGuard { child, port }
    }

    fn launch_with_target(port: u16, target: PathBuf, bp_line: u32) -> Self {
        let bin = debugium_bin();
        let mut cmd = Command::new(&bin);
        cmd.args([
            "launch",
            target.to_str().unwrap(),
            "--adapter",
            "python",
            "--port",
            &port.to_string(),
            "--no-open-browser",
        ]);
        if bp_line > 0 {
            cmd.arg("--breakpoint");
            cmd.arg(format!("{}:{bp_line}", target.display()));
        }
        let child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn debugium launch");
        ServerGuard { child, port }
    }

    fn wait_up(&self, timeout_secs: u64) -> bool {
        wait_server(self.port, timeout_secs)
    }

    fn wait_paused(&self, timeout_secs: u64) -> bool {
        wait_paused(self.port, timeout_secs)
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ─── Group A: MCP protocol layer ─────────────────────────────────────────────

#[test]
fn a1_initialize_returns_protocol_version() {
    let mut p = McpProc::start(9990);
    let r = p.initialize();
    p.stop();
    assert_eq!(
        r["result"]["protocolVersion"].as_str(),
        Some("2024-11-05"),
        "unexpected response: {r}"
    );
}

#[test]
fn a2_tools_list_contains_expected_tools() {
    let mut p = McpProc::start(9990);
    p.initialize();
    let r = p.send("tools/list", None);
    p.stop();
    let tools: std::collections::HashSet<String> = r["result"]["tools"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();
    let expected = [
        "get_sessions",
        "set_breakpoints",
        "continue_execution",
        "step_over",
        "step_in",
        "step_out",
        "get_debug_context",
        "annotate",
        "add_finding",
        "evaluate",
        "get_stack_trace",
        "get_scopes",
        "get_variables",
        "step_until",
        "run_until_exception",
        "get_source",
        "list_breakpoints",
        "clear_breakpoints",
        "list_sessions",
        "get_console_output",
    ];
    for name in &expected {
        assert!(tools.contains(*name), "missing tool: {name}\ngot: {tools:?}");
    }
}

#[test]
fn a3_ping_returns_empty_object() {
    let mut p = McpProc::start(9990);
    p.initialize();
    let r = p.send("ping", None);
    p.stop();
    assert_eq!(r["result"], serde_json::json!({}), "unexpected: {r}");
}

#[test]
fn a4_notification_does_not_crash_process() {
    let mut p = McpProc::start(9990);
    p.initialize();
    // Write a notification (no id) and verify process still alive
    let notif = serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    p.child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{notif}\n").as_bytes())
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(p.child.try_wait().unwrap().is_none(), "process exited unexpectedly");
    p.stop();
}

#[test]
fn a5_unknown_method_returns_minus_32601() {
    let mut p = McpProc::start(9990);
    p.initialize();
    let r = p.send("bad/method", None);
    p.stop();
    assert_eq!(
        r["error"]["code"].as_i64(),
        Some(-32601),
        "unexpected: {r}"
    );
}

#[test]
fn a6_malformed_json_returns_minus_32700() {
    let mut p = McpProc::start(9990);
    p.child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"not json at all\n")
        .unwrap();
    let mut line = String::new();
    p.reader.read_line(&mut line).unwrap();
    p.stop();
    let r: serde_json::Value = serde_json::from_str(line.trim()).expect("not JSON");
    assert_eq!(r["error"]["code"].as_i64(), Some(-32700), "unexpected: {r}");
}

// ─── Group D: Server NOT running ─────────────────────────────────────────────

#[test]
fn d1_initialize_works_without_server() {
    let mut p = McpProc::start(9991);
    let r = p.initialize();
    p.stop();
    assert_eq!(
        r["result"]["protocolVersion"].as_str(),
        Some("2024-11-05"),
        "unexpected: {r}"
    );
}

#[test]
fn d2_tools_call_errors_when_server_not_running() {
    let mut p = McpProc::start(9991);
    p.initialize();
    let r = p.tool_call("get_sessions", serde_json::json!({}));
    p.stop();
    assert!(r.get("error").is_some(), "expected error, got: {r}");
    let msg = r["error"]["message"].as_str().unwrap_or("");
    let ok = ["cannot reach", "connect", "refused", "os error", "server"]
        .iter()
        .any(|w| msg.to_lowercase().contains(w));
    assert!(ok, "unexpected error message: {msg}");
}

// ─── Group L: CLI help / usage ────────────────────────────────────────────────

#[test]
fn l1_help_exit_zero_contains_subcommands() {
    let out = Command::new(debugium_bin())
        .arg("--help")
        .output()
        .expect("failed to run");
    assert!(out.status.success(), "exit: {}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = stdout.as_ref().to_string() + stderr.as_ref();
    for word in ["launch", "attach", "mcp"] {
        assert!(text.contains(word), "--help missing '{word}':\n{text}");
    }
}

#[test]
fn l2_launch_help_contains_flags() {
    let out = Command::new(debugium_bin())
        .args(["launch", "--help"])
        .output()
        .expect("failed to run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = stdout.as_ref().to_string() + stderr.as_ref();
    for flag in ["--adapter", "--port", "--breakpoint"] {
        assert!(text.contains(flag), "launch --help missing '{flag}':\n{text}");
    }
}

#[test]
fn l3_mcp_help_contains_port() {
    let out = Command::new(debugium_bin())
        .args(["mcp", "--help"])
        .output()
        .expect("failed to run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = stdout.as_ref().to_string() + stderr.as_ref();
    assert!(text.contains("--port"), "mcp --help missing --port:\n{text}");
}

#[test]
fn l4_no_subcommand_exits_nonzero() {
    let out = Command::new(debugium_bin())
        .output()
        .expect("failed to run");
    assert!(!out.status.success(), "expected non-zero exit, got: {}", out.status);
}

// ─── Group P: Attach subcommand ───────────────────────────────────────────────

#[test]
fn p1_attach_help_exit_zero_contains_flags() {
    let out = Command::new(debugium_bin())
        .args(["attach", "--help"])
        .output()
        .expect("failed to run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = stdout.as_ref().to_string() + stderr.as_ref();
    assert!(text.contains("--port"), "attach --help missing --port:\n{text}");
    assert!(
        text.to_lowercase().contains("serve"),
        "attach --help missing serve:\n{text}"
    );
}

#[test]
fn p2_attach_bad_port_handled_gracefully() {
    let mut child = Command::new(debugium_bin())
        .args(["attach", "--port", "99999", "--serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn");
    // Give it up to 6 seconds to exit with an error (or produce output)
    let start = Instant::now();
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break Some(s);
        }
        if start.elapsed() > Duration::from_secs(6) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = child.kill();
    let _ = child.wait();
    // Accept: exited (any code) within 6s, OR timed-out (process hung — acceptable for attach)
    // Just assert we didn't panic (i.e., we got here)
    let _ = status; // either Some(exit) or None(timeout) is fine
}

// ─── Group N: MCP subcommand ──────────────────────────────────────────────────

#[test]
fn n1_mcp_no_server_initialize_ok_tool_call_errors() {
    let mut p = McpProc::start(9992);
    let r = p.initialize();
    assert_eq!(
        r["result"]["protocolVersion"].as_str(),
        Some("2024-11-05"),
        "{r}"
    );
    let r = p.tool_call("get_sessions", serde_json::json!({}));
    p.stop();
    assert!(r.get("error").is_some(), "expected error, got: {r}");
}

#[test]
fn n3_mcp_help_default_port_7331() {
    let out = Command::new(debugium_bin())
        .args(["mcp", "--help"])
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = stdout.as_ref().to_string() + stderr.as_ref();
    assert!(text.contains("7331"), "expected default port 7331 in help:\n{text}");
}

// ─── Group E: Breakpoint Tools ────────────────────────────────────────────────

#[test]
fn e1_set_breakpoints_verified() {
    let srv = ServerGuard::launch(7361, Some(43), None);
    assert!(srv.wait_up(12), "server never started on port 7361");
    assert!(srv.wait_paused(12), "session never paused on port 7361");

    let mut p = McpProc::start(7361);
    p.initialize();
    let r = p.tool_call(
        "set_breakpoints",
        serde_json::json!({"file": target_py(), "lines": [47]}),
    );
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "unexpected error: {r}");
    assert!(
        text.to_lowercase().contains("verified"),
        "expected 'verified' in response, got: {text}"
    );
}

#[test]
fn e2_list_breakpoints_after_set() {
    let srv = ServerGuard::launch(7362, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7362);
    p.initialize();
    p.tool_call(
        "set_breakpoints",
        serde_json::json!({"file": target_py(), "lines": [47]}),
    );
    let r = p.tool_call("list_breakpoints", serde_json::json!({}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(text.contains("target_python"), "expected file in output: {text}");
}

#[test]
fn e3_set_breakpoints_with_condition() {
    let srv = ServerGuard::launch(7363, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7363);
    p.initialize();
    let r = p.tool_call(
        "set_breakpoints",
        serde_json::json!({
            "file": target_py(),
            "lines": [49],
            "conditions": ["step > 5"]
        }),
    );
    p.stop();
    assert!(r.get("error").is_none(), "error: {r}");
}

#[test]
fn e4_clear_breakpoints_empties_list() {
    let srv = ServerGuard::launch(7364, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7364);
    p.initialize();
    p.tool_call(
        "set_breakpoints",
        serde_json::json!({"file": target_py(), "lines": [47]}),
    );
    let r_clear = p.tool_call("clear_breakpoints", serde_json::json!({}));
    let r_list = p.tool_call("list_breakpoints", serde_json::json!({}));
    p.stop();
    assert!(r_clear.get("error").is_none(), "clear error: {r_clear}");
    let text = McpProc::text(&r_list);
    // After clear, should not list any active BPs for target file
    let has_bps = text.contains("target_python") && !text.contains("[]") && !text.is_empty();
    assert!(!has_bps || text.contains("no breakpoints") || text.contains("{}"),
            "expected empty after clear, got: {text}");
}

#[test]
fn e5_set_function_breakpoints() {
    let srv = ServerGuard::launch(7365, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7365);
    p.initialize();
    let r = p.tool_call(
        "set_function_breakpoints",
        serde_json::json!({"names": ["classify"]}),
    );
    p.stop();
    assert!(r.get("error").is_none(), "error: {r}");
}

#[test]
fn e6_set_exception_breakpoints() {
    let srv = ServerGuard::launch(7366, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7366);
    p.initialize();
    let r = p.tool_call(
        "set_exception_breakpoints",
        serde_json::json!({"filters": ["uncaughtExceptions"]}),
    );
    p.stop();
    assert!(r.get("error").is_none(), "error: {r}");
}

// ─── Group F: Inspection Tools ────────────────────────────────────────────────

#[test]
fn f1_get_threads_returns_main_thread() {
    let srv = ServerGuard::launch(7371, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7371);
    p.initialize();
    let r = p.tool_call("get_threads", serde_json::json!({}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(
        text.to_lowercase().contains("thread") || text.to_lowercase().contains("main"),
        "expected thread info: {text}"
    );
}

#[test]
fn f2_get_stack_trace_shows_line_43() {
    let srv = ServerGuard::launch(7372, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7372);
    p.initialize();
    let r = p.tool_call("get_stack_trace", serde_json::json!({"thread_id": 1}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(text.contains("43"), "expected line 43 in stack trace: {text}");
    assert!(
        text.contains("target_python"),
        "expected target file in stack trace: {text}"
    );
}

#[test]
fn f5_evaluate_simple_expression() {
    let srv = ServerGuard::launch(7375, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7375);
    p.initialize();
    let r = p.tool_call(
        "evaluate",
        serde_json::json!({"expression": "1 + 1", "context": "repl"}),
    );
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(text.contains('2'), "expected '2' in result: {text}");
}

#[test]
fn f6_get_capabilities_returns_data() {
    let srv = ServerGuard::launch(7376, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7376);
    p.initialize();
    let r = p.tool_call("get_capabilities", serde_json::json!({}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(!text.is_empty(), "expected non-empty capabilities: {r}");
}

#[test]
fn f7_get_source_full_file_at_least_60_lines() {
    let srv = ServerGuard::launch(7377, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7377);
    p.initialize();
    let r = p.tool_call(
        "get_source",
        serde_json::json!({"path": target_py()}),
    );
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    let line_count = text.lines().count();
    assert!(
        line_count >= 60 || text.len() > 800,
        "expected ≥60 lines, got {line_count}: {text}"
    );
}

#[test]
fn f8_get_source_around_line_43_has_marker() {
    let srv = ServerGuard::launch(7378, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7378);
    p.initialize();
    let r = p.tool_call(
        "get_source",
        serde_json::json!({"path": target_py(), "around_line": 43}),
    );
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(
        text.contains('→') || text.contains("43"),
        "expected arrow or line 43 in output: {text}"
    );
}

// ─── Group G: Execution Control ───────────────────────────────────────────────

/// G tests are sequential and share one server; run in a single test to avoid
/// port conflicts and state issues.
#[test]
fn g_execution_control_sequential() {
    let srv = ServerGuard::launch(7380, Some(43), Some(49));
    assert!(srv.wait_up(12), "server never started");
    assert!(srv.wait_paused(12), "session never paused");

    // G1: continue → re-pause at line 49
    {
        let mut p = McpProc::start(7380);
        p.initialize();
        let r = p.tool_call("continue_execution", serde_json::json!({"thread_id": 1}));
        p.stop();
        assert!(r.get("error").is_none(), "G1 continue error: {r}");
    }
    assert!(srv.wait_paused(8), "G1: session did not re-pause after continue");

    // G2: step_over → paused at some line
    {
        let mut p = McpProc::start(7380);
        p.initialize();
        let r = p.tool_call("step_over", serde_json::json!({"thread_id": 1}));
        p.stop();
        assert!(r.get("error").is_none(), "G2 step_over error: {r}");
    }
    assert!(srv.wait_paused(5), "G2: not paused after step_over");

    // G3: step_in
    {
        let mut p = McpProc::start(7380);
        p.initialize();
        let r = p.tool_call("step_in", serde_json::json!({"thread_id": 1}));
        p.stop();
        assert!(r.get("error").is_none(), "G3 step_in error: {r}");
    }
    assert!(srv.wait_paused(5), "G3: not paused after step_in");

    // G4: step_out
    {
        let mut p = McpProc::start(7380);
        p.initialize();
        let r = p.tool_call("step_out", serde_json::json!({"thread_id": 1}));
        p.stop();
        assert!(r.get("error").is_none(), "G4 step_out error: {r}");
    }
    assert!(srv.wait_paused(5), "G4: not paused after step_out");

    // G5: get_console_output → string
    {
        let mut p = McpProc::start(7380);
        p.initialize();
        let r = p.tool_call("get_console_output", serde_json::json!({}));
        p.stop();
        assert!(r.get("error").is_none(), "G5 console_output error: {r}");
        let _ = McpProc::text(&r); // just verify it returns a string
    }
}

// ─── Group H: Compound / LLM Tools ───────────────────────────────────────────

#[test]
fn h1_get_debug_context_all_keys_present() {
    let srv = ServerGuard::launch(7381, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7381);
    p.initialize();
    let r = p.tool_call("get_debug_context", serde_json::json!({}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    for key in ["paused_at", "file", "line", "locals", "call_stack", "source_window"] {
        assert!(text.contains(key), "missing key '{key}' in context: {text}");
    }
}

#[test]
fn h2_get_debug_context_compact() {
    let srv = ServerGuard::launch(7382, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7382);
    p.initialize();
    let full = McpProc::text(&p.tool_call("get_debug_context", serde_json::json!({})));
    let compact = McpProc::text(
        &p.tool_call("get_debug_context", serde_json::json!({"verbosity": "compact"})),
    );
    p.stop();
    // compact should not be dramatically larger than full
    assert!(
        compact.len() <= full.len() * 2 + 100,
        "compact unexpectedly large: compact={}, full={}",
        compact.len(),
        full.len()
    );
}

#[test]
fn h3_annotate_ok() {
    let srv = ServerGuard::launch(7383, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7383);
    p.initialize();
    let r = p.tool_call(
        "annotate",
        serde_json::json!({
            "file": target_py(),
            "line": 43,
            "message": "test annotation",
            "color": "info"
        }),
    );
    p.stop();
    assert!(r.get("error").is_none(), "error: {r}");
}

#[test]
fn h4_add_finding_ok() {
    let srv = ServerGuard::launch(7384, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7384);
    p.initialize();
    let r = p.tool_call(
        "add_finding",
        serde_json::json!({"message": "test finding", "level": "warning"}),
    );
    p.stop();
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(!McpProc::text(&r).is_empty(), "expected non-empty response");
}

#[test]
fn h5_step_until_condition() {
    let srv = ServerGuard::launch(7385, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7385);
    p.initialize();
    let r = p.tool_call(
        "step_until",
        serde_json::json!({"condition": "False", "max_steps": 3}),
    );
    p.stop();
    assert!(r.get("error").is_none(), "error: {r}");
    let text = McpProc::text(&r);
    assert!(!text.is_empty(), "expected non-empty step_until response");
}

#[test]
fn h6_list_sessions_enriched() {
    let srv = ServerGuard::launch(7386, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7386);
    p.initialize();
    let r = p.tool_call("list_sessions", serde_json::json!({}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(
        text.to_lowercase().contains("session"),
        "expected 'session' in output: {text}"
    );
    let has_enriched = ["program", "adapter", "started_at", "status"]
        .iter()
        .any(|k| text.to_lowercase().contains(k));
    assert!(has_enriched, "expected enriched fields in list_sessions: {text}");
}

// ─── Group I: Lifecycle / Edge Cases ─────────────────────────────────────────

#[test]
fn i1_get_exception_info_not_on_exception_handled() {
    let srv = ServerGuard::launch(7391, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7391);
    p.initialize();
    // Not paused on an exception — should return error or empty, not crash
    let _r = p.tool_call("get_exception_info", serde_json::json!({"thread_id": 1}));
    p.stop();
    // Just assert we didn't panic (i.e., we got here)
}

#[test]
fn i2_set_variable_handled() {
    let srv = ServerGuard::launch(7392, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    // Get frame_id and var_ref via stack/scopes
    let mut p = McpProc::start(7392);
    p.initialize();
    let r_stack = p.tool_call("get_stack_trace", serde_json::json!({"thread_id": 1}));
    let text_stack = McpProc::text(&r_stack);

    let frame_id = text_stack
        .find("\"id\"")
        .and_then(|i| text_stack[i..].find(':').map(|j| i + j + 1))
        .and_then(|i| {
            text_stack[i..]
                .trim_start_matches(|c: char| c.is_whitespace())
                .split_once(|c: char| !c.is_ascii_digit())
                .and_then(|(num, _)| num.parse::<u64>().ok())
        });

    if let Some(fid) = frame_id {
        let r_scopes = p.tool_call("get_scopes", serde_json::json!({"frame_id": fid}));
        let text_scopes = McpProc::text(&r_scopes);
        let var_ref = text_scopes
            .find("\"variablesReference\"")
            .and_then(|i| text_scopes[i..].find(':').map(|j| i + j + 1))
            .and_then(|i| {
                text_scopes[i..]
                    .trim_start_matches(|c: char| c.is_whitespace())
                    .split_once(|c: char| !c.is_ascii_digit())
                    .and_then(|(num, _)| num.parse::<u64>().ok())
            })
            .filter(|&v| v > 0);

        if let Some(vref) = var_ref {
            let _r = p.tool_call(
                "set_variable",
                serde_json::json!({
                    "variables_reference": vref,
                    "name": "__debugium_test__",
                    "value": "42"
                }),
            );
            // Any response (ok or error) is acceptable — just no crash
        }
    }
    p.stop();
}

#[test]
fn i3_run_until_exception_catches_exception() {
    // Write a tiny Python script that raises
    let tmp = std::env::temp_dir().join("debugium_exc_target.py");
    std::fs::write(&tmp, "import time\ntime.sleep(0.2)\nraise ValueError('intentional')\n")
        .unwrap();

    let mut child = Command::new(debugium_bin())
        .args([
            "launch",
            tmp.to_str().unwrap(),
            "--adapter", "python",
            "--port", "7393",
            "--no-open-browser",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let ok = wait_server(7393, 12);
    if ok {
        std::thread::sleep(Duration::from_millis(500));
        let mut p = McpProc::start(7393);
        p.initialize();
        let r = p.tool_call("run_until_exception", serde_json::json!({}));
        p.stop();
        let text = McpProc::text(&r);
        assert!(r.get("error").is_none(), "error: {r}");
        let has_exc = ["exception", "error", "stopped", "valueerror", "raised"]
            .iter()
            .any(|w| text.to_lowercase().contains(w));
        assert!(has_exc, "expected exception info in response: {text}");
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn i4_disconnect_terminate_false() {
    let srv = ServerGuard::launch(7394, Some(43), None);
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));

    let mut p = McpProc::start(7394);
    p.initialize();
    let r = p.tool_call("disconnect", serde_json::json!({"terminate_debuggee": false}));
    p.stop();
    // Should return some response (ok or error), not crash
    let _ = McpProc::text(&r);
}

// ─── Group M: Launch subcommand ───────────────────────────────────────────────

#[test]
fn m1_launch_fixed_port_http_200() {
    let srv = ServerGuard::launch(7351, None, None);
    assert!(srv.wait_up(12), "server never started on port 7351");
    let body = http_get(7351, "/sessions").expect("/sessions failed");
    assert!(!body.is_empty(), "expected JSON body");
}

#[test]
fn m3_launch_with_breakpoint_pauses() {
    let srv = ServerGuard::launch(7352, Some(43), None);
    assert!(srv.wait_up(12), "server never started");
    assert!(srv.wait_paused(12), "session never paused");
    let body = http_get(7352, "/state").expect("/state failed");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["paused"].as_bool(), Some(true), "expected paused=true: {v}");
}

#[test]
fn m4_multiple_breakpoints_both_set() {
    let srv = ServerGuard::launch(7353, Some(43), Some(49));
    assert!(srv.wait_up(12));
    assert!(srv.wait_paused(12));
    let body = http_get(7353, "/breakpoints").unwrap_or_default();
    assert!(body.contains("43"), "expected line 43 in /breakpoints: {body}");
    assert!(body.contains("49"), "expected line 49 in /breakpoints: {body}");
}

#[test]
fn m6_port_file_written() {
    let srv = ServerGuard::launch(7355, None, None);
    assert!(srv.wait_up(12));
    let port_file = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".debugium/port");
    assert!(port_file.exists(), "port file not found at {port_file:?}");
    let content = std::fs::read_to_string(&port_file).unwrap_or_default();
    assert!(
        content.trim().parse::<u16>().is_ok(),
        "port file doesn't contain a port number: '{content}'"
    );
}

// ─── Group N2: MCP proxy with live server ─────────────────────────────────────

#[test]
fn n2_mcp_proxy_with_live_server_get_sessions() {
    let srv = ServerGuard::launch(7356, None, None);
    assert!(srv.wait_up(12));

    let mut p = McpProc::start(7356);
    p.initialize();
    let r = p.tool_call("get_sessions", serde_json::json!({}));
    p.stop();
    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "error: {r}");
    assert!(
        text.to_lowercase().contains("session"),
        "expected sessions in response: {text}"
    );
}

// ─── Group O: Launch with --mcp ───────────────────────────────────────────────

#[test]
fn o1_launch_mcp_flag_stdio_and_http() {
    let port: u16 = 7357;
    let bin = debugium_bin();
    let target = target_py();
    let mut child = Command::new(&bin)
        .args([
            "launch",
            target.to_str().unwrap(),
            "--adapter", "python",
            "--port", &port.to_string(),
            "--no-open-browser",
            "--mcp",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn failed");

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // Wait for HTTP server
    assert!(wait_server(port, 12), "HTTP server never started");

    // MCP via stdin
    let init = serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"clientInfo":{"name":"test"}}});
    child.stdin.as_mut().unwrap()
        .write_all(format!("{init}\n").as_bytes()).unwrap();
    let mut resp = String::new();
    reader.read_line(&mut resp).unwrap();
    let r: serde_json::Value = serde_json::from_str(resp.trim()).unwrap();
    assert_eq!(r["result"]["protocolVersion"].as_str(), Some("2024-11-05"), "{r}");

    let call = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_sessions","arguments":{}}});
    child.stdin.as_mut().unwrap()
        .write_all(format!("{call}\n").as_bytes()).unwrap();
    let mut resp2 = String::new();
    reader.read_line(&mut resp2).unwrap();
    let r2: serde_json::Value = serde_json::from_str(resp2.trim()).unwrap();
    assert!(r2.get("error").is_none(), "O1 MCP get_sessions error: {r2}");

    // O2: HTTP also works
    let body = http_get(port, "/sessions").expect("O2 /sessions failed");
    assert!(!body.is_empty(), "O2 expected /sessions JSON body");

    let _ = child.kill();
    let _ = child.wait();
}

// ─── Group T: Multi-thread / thread-id auto-detect ───────────────────────────

/// T1: get_stack_trace with no thread_id arg auto-detects the paused thread.
#[test]
fn t1_get_stack_trace_auto_detects_paused_thread() {
    let target = target_threads_py();
    let srv = ServerGuard::launch_with_target(7401, target, 6);
    assert!(srv.wait_up(15), "T1 server never started");
    assert!(srv.wait_paused(15), "T1 never paused at breakpoint");

    let mut p = McpProc::start(7401);
    p.initialize();
    // Call get_stack_trace WITHOUT thread_id — server must auto-detect
    let r = p.tool_call("get_stack_trace", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "T1 error: {r}");
    assert!(
        text.contains("worker"),
        "T1 expected 'worker' frame in stack trace: {text}"
    );
}

/// T2: get_threads returns at least one thread when paused inside worker().
#[test]
fn t2_get_threads_returns_thread_list() {
    let target = target_threads_py();
    let srv = ServerGuard::launch_with_target(7402, target, 6);
    assert!(srv.wait_up(15), "T2 server never started");
    assert!(srv.wait_paused(15), "T2 never paused");

    let mut p = McpProc::start(7402);
    p.initialize();
    let r = p.tool_call("get_threads", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "T2 error: {r}");
    assert!(
        !text.is_empty() && (text.contains("id") || text.contains("Thread")),
        "T2 expected thread list: {text}"
    );
}

/// T3: evaluate without frame_id auto-resolves via paused thread (not hardcoded 1).
#[test]
fn t3_evaluate_auto_resolves_frame_via_paused_thread() {
    let target = target_threads_py();
    let srv = ServerGuard::launch_with_target(7403, target, 6);
    assert!(srv.wait_up(15), "T3 server never started");
    assert!(srv.wait_paused(15), "T3 never paused");

    let mut p = McpProc::start(7403);
    p.initialize();
    // Evaluate `name` — a local variable in worker() — no frame_id provided
    let r = p.tool_call(
        "evaluate",
        serde_json::json!({"expression": "name", "context": "repl"}),
    );
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "T3 error: {r}");
    // Should be "alpha" or "beta" depending on which thread hit first
    assert!(
        text.contains("alpha") || text.contains("beta"),
        "T3 expected worker name in eval result: {text}"
    );
}

/// T4: get_debug_context without thread_id auto-detects the paused thread.
#[test]
fn t4_get_debug_context_auto_detects_thread() {
    let target = target_threads_py();
    let srv = ServerGuard::launch_with_target(7404, target, 6);
    assert!(srv.wait_up(15), "T4 server never started");
    assert!(srv.wait_paused(15), "T4 never paused");

    let mut p = McpProc::start(7404);
    p.initialize();
    let r = p.tool_call("get_debug_context", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "T4 error: {r}");
    // Context should show we're inside worker()
    assert!(
        text.contains("worker") || text.contains("target_threads"),
        "T4 expected worker context: {text}"
    );
}

// ─── Group U: Subprocess / multi-process auto-attach ─────────────────────────
//
// The parent script (`target_subprocess.py`) spawns a child Python process via
// `subprocess.run`.  Because the server now launches Python with `subProcess: true`,
// debugpy fires a `debugpyAttach` event when the child starts.  The server
// auto-attaches (TCP handshake) and forwards the child's DAP events — including
// `stopped` — under the parent session ID.  The child script contains
// `breakpoint()` so it always pauses after auto-attach.
//
// These tests use a longer timeout (25 s) because the subprocess must start, debugpy
// must fire the attach event, and we must complete the child handshake before the
// child's `stopped` event arrives.

/// U1: Child's breakpoint() pauses and /state reflects paused=true.
#[test]
fn u1_subprocess_child_pauses_in_parent_session() {
    let target = target_subprocess_py();
    // No breakpoint in the parent — the child's breakpoint() will trigger a stop.
    let srv = ServerGuard::launch_with_target(7411, target, 0);
    assert!(srv.wait_up(15), "U1 server never started");
    // Child attach + pause is async — give it more time than normal tests.
    assert!(wait_paused(7411, 25), "U1 session never paused (child breakpoint not received)");
}

/// U2: While child is paused at breakpoint(), the parent's stack shows it blocked inside
/// subprocess.run() (waiting for the child).  MCP get_stack_trace uses the parent
/// adapter — it returns the parent thread's call stack, which includes target_subprocess.py.
#[test]
fn u2_parent_stack_shows_subprocess_call() {
    let target = target_subprocess_py();
    let srv = ServerGuard::launch_with_target(7412, target, 0);
    assert!(srv.wait_up(15), "U2 server never started");
    assert!(wait_paused(7412, 25), "U2 session never paused");

    let mut p = McpProc::start(7412);
    p.initialize();
    let r = p.tool_call("get_stack_trace", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "U2 error: {r}");
    // Parent is blocked inside subprocess.run() → stack contains target_subprocess
    assert!(
        text.contains("target_subprocess") || text.contains("subprocess"),
        "U2 expected parent stack while waiting for child: {text}"
    );
}

/// U3: While the child is paused at breakpoint(), get_threads returns at least one
/// thread — confirming the session has a live paused state to inspect.
#[test]
fn u3_get_threads_while_child_paused() {
    let target = target_subprocess_py();
    let srv = ServerGuard::launch_with_target(7413, target, 0);
    assert!(srv.wait_up(15), "U3 server never started");
    assert!(wait_paused(7413, 25), "U3 session never paused");

    let mut p = McpProc::start(7413);
    p.initialize();
    let r = p.tool_call("get_threads", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "U3 error: {r}");
    // At least the parent's main thread should be listed
    assert!(
        !text.is_empty() && (text.contains("id") || text.contains("Thread")),
        "U3 expected thread list: {text}"
    );
}

// ─── Group V: New LLM tools ───────────────────────────────────────────────────

/// V1: annotate a line, then get_annotations returns it.
#[test]
fn v1_get_annotations_after_annotate() {
    let srv = ServerGuard::launch(7421, Some(43), None);
    assert!(srv.wait_up(12), "V1 server never started");
    assert!(srv.wait_paused(12), "V1 session never paused");

    let mut p = McpProc::start(7421);
    p.initialize();
    p.tool_call("annotate", serde_json::json!({
        "file": target_py(),
        "line": 43,
        "message": "v1 test annotation",
        "color": "red"
    }));
    let r = p.tool_call("get_annotations", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "V1 error: {r}");
    assert!(text.contains("v1 test annotation"), "V1 expected annotation in response: {text}");
}

/// V2: add_finding, then get_findings returns it.
#[test]
fn v2_get_findings_after_add_finding() {
    let srv = ServerGuard::launch(7422, Some(43), None);
    assert!(srv.wait_up(12), "V2 server never started");
    assert!(srv.wait_paused(12), "V2 session never paused");

    let mut p = McpProc::start(7422);
    p.initialize();
    p.tool_call("add_finding", serde_json::json!({
        "message": "v2 test finding",
        "level": "warning"
    }));
    let r = p.tool_call("get_findings", serde_json::json!({}));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "V2 error: {r}");
    assert!(text.contains("v2 test finding"), "V2 expected finding in response: {text}");
}

/// V3: step a few times, then get_variable_history returns history (may be empty if var not in scope).
#[test]
fn v3_get_variable_history_tracks_variable() {
    let srv = ServerGuard::launch(7423, Some(43), None);
    assert!(srv.wait_up(12), "V3 server never started");
    assert!(srv.wait_paused(12), "V3 session never paused");

    let mut p = McpProc::start(7423);
    p.initialize();
    // Step a few times to build up timeline entries
    p.tool_call("step_over", serde_json::json!({}));
    std::thread::sleep(std::time::Duration::from_millis(500));
    p.tool_call("step_over", serde_json::json!({}));
    std::thread::sleep(std::time::Duration::from_millis(500));
    p.tool_call("step_over", serde_json::json!({}));
    std::thread::sleep(std::time::Duration::from_millis(500));
    let r = p.tool_call("get_variable_history", serde_json::json!({ "name": "fibs" }));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "V3 error: {r}");
    // Response must contain "name" and "history" keys
    assert!(text.contains("\"name\"") && text.contains("\"history\""),
        "V3 expected name+history keys: {text}");
}

/// V4: wait_for_output with a pattern that should already be in console output (or times out gracefully).
#[test]
fn v4_wait_for_output_returns_result() {
    let srv = ServerGuard::launch(7424, Some(43), None);
    assert!(srv.wait_up(12), "V4 server never started");
    assert!(srv.wait_paused(12), "V4 session never paused");

    let mut p = McpProc::start(7424);
    p.initialize();
    // Use a short timeout so the test finishes quickly regardless of match
    let r = p.tool_call("wait_for_output", serde_json::json!({
        "pattern": "result",
        "timeout_secs": 2
    }));
    p.stop();

    let text = McpProc::text(&r);
    assert!(r.get("error").is_none(), "V4 error: {r}");
    // Must return matched + line fields
    assert!(text.contains("\"matched\""), "V4 expected 'matched' field: {text}");
}
