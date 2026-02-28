//! Debugium Rust test target.
//! Run via:  debugium launch tests/target_rust/src/main.rs --adapter lldb --serve
//!
//! Build first:  cargo build --manifest-path tests/target_rust/Cargo.toml

use std::collections::HashMap;

// ── Types ─────────────────────────────────────────────────────── //

#[derive(Debug, Clone)]
struct DebugSession {
    id: String,
    adapter: String,
    paused: bool,
    thread_id: Option<u32>,
    stack_depth: usize,
}

impl DebugSession {
    fn new(id: &str, adapter: &str) -> Self {
        Self {
            id: id.to_string(),
            adapter: adapter.to_string(),
            paused: false,
            thread_id: None,
            stack_depth: 0,
        }
    }

    fn pause(&mut self, thread_id: u32) {
        self.paused = true;
        self.thread_id = Some(thread_id);
    }
}

// ── Fibonacci ─────────────────────────────────────────────────── //

fn fibonacci(n: usize) -> Vec<u64> {
    let mut seq = vec![0u64, 1];
    for _ in 0..n.saturating_sub(2) {
        let l = seq.len();
        seq.push(seq[l - 1] + seq[l - 2]);
    }
    seq.truncate(n);
    seq
}

fn classify(n: u64) -> &'static str {
    match (n % 3, n % 5) {
        (0, 0) => "fizzbuzz",
        (0, _) => "fizz",
        (_, 0) => "buzz",
        _      => "num",
    }
}

// ── Main ──────────────────────────────────────────────────────── //

fn main() {
    // Breakpoint target 1: fibonacci + classify
    let fibs = fibonacci(12);
    let labels: Vec<(&u64, &str)> = fibs.iter().map(|n| (n, classify(*n))).collect();
    println!("Fibonacci labels: {:?}", labels);

    // Breakpoint target 2: struct + mutation
    let mut session = DebugSession::new("rs-1", "lldb");
    session.pause(1);
    session.stack_depth = 3;
    println!("Session: {:?}", session);

    // Breakpoint target 3: HashMap + iterators
    let mut metadata: HashMap<&str, String> = HashMap::new();
    metadata.insert("tool", "debugium".to_string());
    metadata.insert("adapter", session.adapter.clone());
    metadata.insert("status", if session.paused { "paused".into() } else { "running".into() });

    let total: u64 = fibs.iter().sum();
    metadata.insert("fib_sum", total.to_string());

    for (k, v) in &metadata {
        println!("  {k}: {v}");
    }

    println!("Done. sum={total}");
}
