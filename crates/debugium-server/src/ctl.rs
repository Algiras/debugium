use anyhow::Result;
use serde_json::{json, Value};

// ── Port resolution ──────────────────────────────────────────────────────────

pub fn resolve_port(opt: Option<u16>) -> Result<u16> {
    if let Some(p) = opt {
        return Ok(p);
    }
    let home = crate::home::DebugiumHome::open()?;
    home.read_port().ok_or_else(|| anyhow::anyhow!(
        "No running Debugium server found (~/.debugium/port missing or stale).\n\
         Start one with: debugium launch <program>"
    ))
}

// ── Path helpers ─────────────────────────────────────────────────────────────

fn short_path(path: &str) -> &str {
    // Show just filename for display; full path in --json mode
    path.rsplit('/').next().unwrap_or(path)
}

// ── HTTP helper ──────────────────────────────────────────────────────────────

async fn call(port: u16, tool: &str, args: Value) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .post(format!("http://127.0.0.1:{port}/mcp-proxy"))
        .json(&json!({ "tool": tool, "args": args }))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                anyhow::anyhow!("Cannot connect to Debugium server on port {port}. Is it running?")
            } else if e.is_timeout() {
                anyhow::anyhow!("Request to Debugium server timed out (port {port})")
            } else {
                anyhow::anyhow!("HTTP error: {e}")
            }
        })?;
    let body: Value = resp.json().await?;
    if body["ok"].as_bool() == Some(true) {
        Ok(body["result"].as_str().unwrap_or("{}").to_string())
    } else {
        Err(anyhow::anyhow!(
            "{}",
            body["error"].as_str().unwrap_or("unknown error")
        ))
    }
}

// ── Output helper ────────────────────────────────────────────────────────────

fn output(raw: &str, as_json: bool, format: impl FnOnce(&Value)) {
    if as_json {
        println!("{raw}");
        return;
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => format(&v),
        Err(_) => println!("{raw}"),
    }
}

// ── Subcommand dispatch ───────────────────────────────────────────────────────

pub struct Opts {
    pub port: Option<u16>,
    pub session: String,
    pub json: bool,
}

pub async fn sessions(opts: Opts) -> Result<()> {
    let port = resolve_port(opts.port)?;
    // list_sessions doesn't use session arg; pass empty object
    let raw = call(port, "list_sessions", json!({})).await?;
    output(&raw, opts.json, |v| {
        // Returns {"sessions": [...]}
        let arr = v["sessions"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
        if arr.is_empty() {
            println!("  (no sessions)");
        }
        for s in arr {
            let id = s["id"].as_str().unwrap_or("?");
            let adapter = s["adapter"].as_str().unwrap_or("?");
            let program = s["program"].as_str().unwrap_or("?");
            let status = s.get("status").and_then(Value::as_str).unwrap_or("running");
            println!("  [{id}]  {adapter}  {}  {status}", short_path(program));
        }
    });
    Ok(())
}

pub async fn threads(opts: Opts) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let raw = call(
        port,
        "get_threads",
        json!({ "session": opts.session }),
    )
    .await?;
    output(&raw, opts.json, |v| {
        if let Some(arr) = v.as_array() {
            for t in arr {
                let id = &t["id"];
                let name = t["name"].as_str().unwrap_or("?");
                println!("  [{id}]  {name}");
            }
        } else {
            println!("{v}");
        }
    });
    Ok(())
}

pub async fn stack(opts: Opts, thread_id: u64) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let raw = call(
        port,
        "get_stack_trace",
        // MCP tool expects `thread_id` (snake_case)
        json!({ "session": opts.session, "thread_id": thread_id }),
    )
    .await?;
    output(&raw, opts.json, |v| {
        if let Some(arr) = v.as_array() {
            for (i, f) in arr.iter().enumerate() {
                let name = f["name"].as_str().unwrap_or("?");
                let file = f
                    .get("source")
                    .and_then(|s| s["path"].as_str())
                    .unwrap_or("?");
                let line = f["line"].as_u64().unwrap_or(0);
                println!("  #{i}  {name}  {file}:{line}");
            }
        } else {
            println!("{v}");
        }
    });
    Ok(())
}

pub async fn bp_set(opts: Opts, locations: Vec<String>) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let mut map: std::collections::HashMap<String, Vec<u64>> =
        std::collections::HashMap::new();
    for loc in &locations {
        if let Some((file, line_str)) = loc.rsplit_once(':') {
            if let Ok(line) = line_str.parse::<u64>() {
                map.entry(file.to_string()).or_default().push(line);
            }
        }
    }
    for (file, lines) in map {
        let raw = call(
            port,
            "set_breakpoints",
            json!({ "session": opts.session, "file": file, "lines": lines }),
        )
        .await?;
        if opts.json {
            println!("{raw}");
        } else {
            println!("  ✓ breakpoints set: {file}:{lines:?}");
        }
    }
    Ok(())
}

pub async fn bp_list(opts: Opts) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let raw = call(
        port,
        "list_breakpoints",
        json!({ "session": opts.session }),
    )
    .await?;
    output(&raw, opts.json, |v| {
        // Returns {"breakpoints": {file: [BpSpec, ...]}} where BpSpec is {"line":N} or {"line":N,"condition":"..."}
        if let Some(obj) = v["breakpoints"].as_object() {
            if obj.is_empty() {
                println!("  (no breakpoints)");
            }
            for (file, specs) in obj {
                let ls: Vec<String> = specs
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .map(|bp| {
                        if let Some(line) = bp["line"].as_u64() {
                            if let Some(cond) = bp.get("condition").and_then(Value::as_str) {
                                format!("{line} if {cond}")
                            } else {
                                line.to_string()
                            }
                        } else if let Some(n) = bp.as_u64() {
                            n.to_string()
                        } else {
                            bp.to_string()
                        }
                    })
                    .collect();
                println!("  {}: {}", short_path(file), ls.join(", "));
            }
        } else {
            println!("{v}");
        }
    });
    Ok(())
}

pub async fn bp_clear(opts: Opts) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let raw = call(
        port,
        "clear_breakpoints",
        json!({ "session": opts.session }),
    )
    .await?;
    if opts.json {
        println!("{raw}");
    } else {
        println!("  ✓ {raw}");
    }
    Ok(())
}

pub async fn resume(opts: Opts, thread_id: u64) -> Result<()> {
    let port = resolve_port(opts.port)?;
    // tool is `continue_execution`, arg is `thread_id`
    let raw = call(
        port,
        "continue_execution",
        json!({ "session": opts.session, "thread_id": thread_id }),
    )
    .await?;
    if opts.json {
        println!("{raw}");
    } else {
        println!("  ▶ resumed");
    }
    Ok(())
}

pub async fn step(opts: Opts, kind: &str, thread_id: u64) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let tool = match kind {
        "in" => "step_in",
        "out" => "step_out",
        _ => "step_over",
    };
    let raw = call(
        port,
        tool,
        json!({ "session": opts.session, "thread_id": thread_id }),
    )
    .await?;
    if opts.json {
        println!("{raw}");
    } else {
        println!("  → stepped ({kind})");
    }
    Ok(())
}

/// Smart vars: get top frame → scopes → locals variables_reference → variables
pub async fn vars(opts: Opts, frame_id: Option<u64>) -> Result<()> {
    let port = resolve_port(opts.port)?;

    // Resolve frame_id: use provided, or get top of stack
    let fid: u64 = if let Some(f) = frame_id {
        f
    } else {
        let frames_raw = call(
            port,
            "get_stack_trace",
            json!({ "session": opts.session }),
        )
        .await?;
        let frames: Value = serde_json::from_str(&frames_raw)?;
        frames
            .as_array()
            .and_then(|a| a.first())
            .and_then(|f| f["id"].as_u64())
            .unwrap_or(1)
    };

    // Get scopes for that frame
    let scopes_raw = call(
        port,
        "get_scopes",
        json!({ "session": opts.session, "frame_id": fid }),
    )
    .await?;
    let scopes: Value = serde_json::from_str(&scopes_raw)?;

    // Find locals scope (first non-expensive one, prefer name containing "local")
    let vref = scopes
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|s| {
                    s["name"]
                        .as_str()
                        .map(|n| n.to_lowercase().contains("local"))
                        .unwrap_or(false)
                })
                .or_else(|| arr.first())
        })
        .and_then(|s| s["variablesReference"].as_u64())
        .unwrap_or(0);

    if vref == 0 {
        println!("  (no variables)");
        return Ok(());
    }

    let raw = call(
        port,
        "get_variables",
        json!({ "session": opts.session, "variables_reference": vref }),
    )
    .await?;
    output(&raw, opts.json, |v| {
        if let Some(arr) = v.as_array() {
            if arr.is_empty() {
                println!("  (no variables)");
            }
            for var in arr {
                let name = var["name"].as_str().unwrap_or("?");
                let value = var["value"].as_str().unwrap_or("?");
                println!("  {name} = {value}");
            }
        } else {
            println!("{v}");
        }
    });
    Ok(())
}

pub async fn eval(opts: Opts, expression: String, frame_id: Option<u64>) -> Result<()> {
    let port = resolve_port(opts.port)?;

    // frame_id is required by the MCP tool — resolve top frame if not given
    let fid: u64 = if let Some(f) = frame_id {
        f
    } else {
        let frames_raw = call(
            port,
            "get_stack_trace",
            json!({ "session": opts.session }),
        )
        .await?;
        let frames: Value = serde_json::from_str(&frames_raw)?;
        frames
            .as_array()
            .and_then(|a| a.first())
            .and_then(|f| f["id"].as_u64())
            .unwrap_or(1)
    };

    let raw = call(
        port,
        "evaluate",
        json!({ "session": opts.session, "expression": expression, "frame_id": fid }),
    )
    .await?;
    output(&raw, opts.json, |v| {
        let result = v.as_str().unwrap_or_else(|| v["result"].as_str().unwrap_or("?"));
        println!("  = {result}");
    });
    Ok(())
}

pub async fn source(opts: Opts, path: String, line: Option<u32>) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let mut args = json!({ "session": opts.session, "path": path });
    if let Some(l) = line {
        // MCP tool uses `around_line`
        args["around_line"] = json!(l);
    }
    let raw = call(port, "get_source", args).await?;
    // get_source returns a plain string (not JSON)
    if opts.json {
        println!("{}", serde_json::to_string(&raw).unwrap_or(raw));
    } else {
        println!("{raw}");
    }
    Ok(())
}

pub async fn context(opts: Opts, compact: bool) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let verbosity = if compact { "compact" } else { "full" };
    let raw = call(
        port,
        "get_debug_context",
        json!({ "session": opts.session, "verbosity": verbosity }),
    )
    .await?;
    output(&raw, opts.json, |v| {
        let paused = v["paused_at"].as_str().unwrap_or("");
        let stack = v["call_stack"].as_array();
        let has_stack = stack.map(|a| !a.is_empty()).unwrap_or(false);

        if paused.is_empty() && !has_stack {
            println!("  (program not paused — may have terminated)");
            return;
        }

        if !paused.is_empty() {
            println!("── Paused at ──");
            println!("  {paused}");
        }
        if let Some(arr) = stack {
            println!("── Stack ──");
            for (i, f) in arr.iter().enumerate() {
                println!("  #{i}  {f}", f = f.as_str().unwrap_or("?"));
            }
        }
        if let Some(locals) = v["locals"].as_object() {
            if !locals.is_empty() {
                println!("── Locals ──");
                for (name, value) in locals {
                    println!("  {name} = {value}");
                }
            }
        }
        if let Some(src) = v["source_window"].as_str() {
            if !src.is_empty() {
                println!("── Source ──");
                println!("{src}");
            }
        }
        if let Some(bps) = v["breakpoints"].as_object() {
            if !bps.is_empty() {
                println!("── Breakpoints ──");
                for (file, lines) in bps {
                    let ls: Vec<String> = lines
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(|bp| {
                            bp["line"].as_u64().map(|n| n.to_string())
                                .or_else(|| bp.as_u64().map(|n| n.to_string()))
                                .unwrap_or_else(|| bp.to_string())
                        })
                        .collect();
                    println!("  {}: {}", short_path(file), ls.join(", "));
                }
            }
        }
    });
    Ok(())
}

pub async fn annotate(
    opts: Opts,
    location: String,
    message: String,
    color: Option<String>,
) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let (file, line) = location
        .rsplit_once(':')
        .map(|(f, l)| (f.to_string(), l.parse::<u32>().unwrap_or(0)))
        .unwrap_or((location, 0));
    let mut args =
        json!({ "session": opts.session, "file": file, "line": line, "message": message });
    if let Some(c) = color {
        args["color"] = json!(c);
    }
    let raw = call(port, "annotate", args).await?;
    if opts.json {
        println!("{raw}");
    } else {
        println!("  ✓ annotation added");
    }
    Ok(())
}

pub async fn finding(opts: Opts, message: String, level: Option<String>) -> Result<()> {
    let port = resolve_port(opts.port)?;
    let mut args = json!({ "session": opts.session, "message": message });
    if let Some(l) = level {
        args["level"] = json!(l);
    }
    let raw = call(port, "add_finding", args).await?;
    if opts.json {
        println!("{raw}");
    } else {
        println!("  ✓ finding added");
    }
    Ok(())
}
