/// MCP (Model Context Protocol) stdio server.
///
/// Implements MCP 2025-11-25 over stdin/stdout JSON-RPC 2.0.
/// Supports optional elicitation (form/url) when the connecting client
/// declares the capability — otherwise those code paths are no-ops.
///
/// ## Protocol flow
///   stdin  → MCP/JSON-RPC requests from the LLM host  
///   stdout → MCP/JSON-RPC responses / notifications / server-initiated requests
///   stderr → tracing logs (so as not to pollute the protocol stream)

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tracing::debug;

use crate::dap::session::SessionRegistry;
use crate::server::hub::Hub;

// ─── Wire types ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcResponse {
    fn ok(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    fn err(id: Option<Value>, code: i32, msg: impl Into<String>) -> Self {
        Self { jsonrpc: "2.0", id, result: None, error: Some(RpcError { code, message: msg.into() }) }
    }
}

// ─── Tool definitions ────────────────────────────────────────────────────────

/// Maps tool names to the DAP capability they require.
/// Tools not in this list are always included.
const CAPABILITY_GATED_TOOLS: &[(&str, &str)] = &[
    ("read_memory",              "supportsReadMemoryRequest"),
    ("write_memory",             "supportsWriteMemoryRequest"),
    ("disassemble",              "supportsDisassembleRequest"),
    ("set_function_breakpoints", "supportsFunctionBreakpoints"),
    ("set_variable",             "supportsSetVariable"),
    ("restart",                  "supportsRestartRequest"),
    ("terminate",                "supportsTerminateRequest"),
    ("get_exception_info",       "supportsExceptionInfoRequest"),
];

fn tool_list(adapter_caps: &Value) -> Value {
    let has_session = adapter_caps.is_object() && !adapter_caps.as_object().unwrap().is_empty();

    let all_tools = json!([
            {
                "name": "get_sessions",
                "description": "List all active debug sessions.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "set_breakpoints",
                "description": "Set breakpoints in a source file. Replaces all existing breakpoints in that file. The web UI updates immediately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID (from get_sessions). Defaults to 'default'." },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "lines": { "type": "array", "items": { "type": "integer" }, "description": "Line numbers to break on (1-indexed)." }
                    },
                    "required": ["file", "lines"]
                }
            },
            {
                "name": "list_breakpoints",
                "description": "List all currently set breakpoints for a session, grouped by file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "clear_breakpoints",
                "description": "Remove all breakpoints from all files in a session. The web UI updates immediately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "continue_execution",
                "description": "Resume execution until the next breakpoint or program end.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to continue. Omit to continue all threads." }
                    },
                    "required": []
                }
            },
            {
                "name": "step_over",
                "description": "Execute the next line, stepping over function calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "step_in",
                "description": "Step into the function call on the current line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "step_out",
                "description": "Step out of the current function, returning to the caller.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "pause",
                "description": "Pause execution of a running thread.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "get_threads",
                "description": "Get all threads in the current process.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "get_stack_trace",
                "description": "Get the call stack for a thread. Use when the session is paused.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer" },
                        "depth": { "type": "integer", "description": "Max frames to return. Default 20." }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "get_scopes",
                "description": "Get variable scopes for a stack frame (Locals, Globals, etc.).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "frame_id": { "type": "integer", "description": "Frame ID from get_stack_trace." }
                    },
                    "required": ["frame_id"]
                }
            },
            {
                "name": "get_variables",
                "description": "Get variables from a scope or expand a structured variable.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "variables_reference": { "type": "integer", "description": "variablesReference from get_scopes or a variable with children." }
                    },
                    "required": ["variables_reference"]
                }
            },
            {
                "name": "evaluate",
                "description": "Evaluate an expression in the context of a stack frame. Returns the result value.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "expression": { "type": "string", "description": "Expression to evaluate (e.g. 'len(my_list)', 'counter.value')." },
                        "frame_id": { "type": "integer", "description": "Stack frame context. Required for local variable access." },
                        "context": { "type": "string", "enum": ["watch", "repl", "hover", "clipboard"], "description": "Evaluation context. Default: 'repl'." }
                    },
                    "required": ["expression", "frame_id"]
                }
            },
            {
                "name": "get_source",
                "description": "Get the source code of a file, optionally highlighting the current execution line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the source file." },
                        "around_line": { "type": "integer", "description": "If set, returns only the N lines around this line number." },
                        "context_lines": { "type": "integer", "description": "Number of lines of context above/below around_line. Default 10." }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "get_debug_context",
                "description": "Get a compact, LLM-optimized snapshot of the current debug state in one call: paused location, local variables, call stack summary, source window (±5 lines), and active breakpoints. Use this instead of separate get_stack_trace + get_scopes + get_variables calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to inspect. Default 1." },
                        "verbosity": { "type": "string", "enum": ["compact", "full"], "description": "compact = top 10 vars + 3 frames; full = top 30 vars + all frames. Default: full." }
                    },
                    "required": []
                }
            },
            {
                "name": "annotate",
                "description": "Pin a visible note to a specific line in the source editor. The annotation appears as a colored gutter marker that the user can see immediately. Use this to mark suspicious lines, document findings inline, or flag areas of interest.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number (1-indexed)." },
                        "message": { "type": "string", "description": "The annotation text shown on hover." },
                        "color": { "type": "string", "enum": ["warning", "error", "info", "success"], "description": "Gutter marker color. Default: warning." }
                    },
                    "required": ["file", "line", "message"]
                }
            },
            {
                "name": "add_finding",
                "description": "Add a structured finding to the Findings panel visible in the UI. Use this to record conclusions, root causes, hypotheses, or important discoveries during a debugging session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "message": { "type": "string", "description": "The finding text." },
                        "level": { "type": "string", "enum": ["info", "warning", "bug", "fixed"], "description": "Severity / type. Default: info." }
                    },
                    "required": ["message"]
                }
            },
            {
                "name": "step_until",
                "description": "Step over repeatedly until a Python/JS expression evaluates to truthy, or until max_steps is reached. Returns the debug context at the stopping point. Much more efficient than calling step_over + evaluate in a loop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "condition": { "type": "string", "description": "Expression to evaluate after each step (e.g. 'counter.value > 50', 'result != None')." },
                        "max_steps": { "type": "integer", "description": "Maximum steps before giving up. Default 20." },
                        "thread_id": { "type": "integer", "description": "Thread to step. Default 1." }
                    },
                    "required": ["condition"]
                }
            },
            {
                "name": "run_until_exception",
                "description": "Continue execution until an exception is raised, then return full debug context at the crash site. Sets 'raised' exception breakpoints, continues, and returns the exception info + locals in one call.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to watch. Default 1." },
                        "timeout_secs": { "type": "integer", "description": "Max seconds to wait. Default 30." }
                    },
                    "required": []
                }
            },
            {
                "name": "disconnect",
                "description": "Terminate the debug session and the target process.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "terminate_debuggee": { "type": "boolean", "description": "Also kill the target process. Default true." }
                    },
                    "required": []
                }
            },
            {
                "name": "terminate",
                "description": "Send a terminate request to gracefully end the debuggee process.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "restart",
                "description": "Restart the debug session (restarts the debuggee process).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "set_exception_breakpoints",
                "description": "Configure which exception types cause the debugger to pause.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "filters": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["raised", "uncaught", "userUnhandled"] },
                            "description": "Exception filter names. Common: 'uncaught', 'raised'."
                        }
                    },
                    "required": ["filters"]
                }
            },
            {
                "name": "get_capabilities",
                "description": "Get the adapter capabilities returned during initialize. Useful to check if features like function breakpoints, exception info, or completions are supported.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "get_exception_info",
                "description": "Get detailed information about the last exception that caused the debugger to stop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread ID (from get_threads). Required." }
                    },
                    "required": ["thread_id"]
                }
            },
            {
                "name": "set_variable",
                "description": "Change the value of a variable while the debugger is paused.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "variables_reference": { "type": "integer", "description": "variablesReference of the scope/object that contains the variable." },
                        "name": { "type": "string", "description": "Variable name to set." },
                        "value": { "type": "string", "description": "New value as a string expression." }
                    },
                    "required": ["variables_reference", "name", "value"]
                }
            },
            {
                "name": "set_breakpoint",
                "description": "Set a single breakpoint at a specific file and line. Optionally specify a condition expression. Returns the verified line number from the adapter.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number to break on (1-indexed)." },
                        "condition": { "type": "string", "description": "Optional condition expression — only pause when this evaluates to true." }
                    },
                    "required": ["file", "line"]
                }
            },
            {
                "name": "get_console_output",
                "description": "Return recent stdout/stderr output from the debuggee process (last N lines). Useful for reading program output without UI access.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "lines": { "type": "integer", "description": "Maximum number of recent lines to return. Default 50." }
                    },
                    "required": []
                }
            },
            {
                "name": "list_sessions",
                "description": "List all active debug sessions with enriched metadata: program, adapter, status, port, and start time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "set_function_breakpoints",
                "description": "Set breakpoints on function names rather than source lines.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "names": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Function names to break on (e.g. 'MyClass.method', 'free_fn')."
                        }
                    },
                    "required": ["names"]
                }
            },
            {
                "name": "get_timeline",
                "description": "Return the execution timeline: one entry per `stopped` event, oldest first. Each entry includes file, line, timestamp, local variable snapshot, names of changed variables vs previous stop, and a stack summary. Useful for understanding *when* a value went wrong.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "limit": { "type": "integer", "description": "Maximum entries to return (default 50, max 500), oldest first." }
                    },
                    "required": []
                }
            },
            {
                "name": "add_watch",
                "description": "Add a watch expression that is evaluated automatically at every stop and broadcast to the UI. Results are visible in the Watch panel and via get_watches.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "expression": { "type": "string", "description": "Expression to watch (e.g. 'len(cache)', 'x + y')." }
                    },
                    "required": ["expression"]
                }
            },
            {
                "name": "remove_watch",
                "description": "Remove a previously added watch expression.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "expression": { "type": "string", "description": "Expression to remove." }
                    },
                    "required": ["expression"]
                }
            },
            {
                "name": "get_watches",
                "description": "Return current watch expressions and their last evaluated values.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "get_annotations",
                "description": "Return all annotations pinned to source lines in this session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "get_findings",
                "description": "Return all findings (bug reports / conclusions) added in this session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "get_variable_history",
                "description": "Show how a local variable's value changed across all timeline stops. Useful for 'when did X become wrong?'",
                "inputSchema": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string", "description": "Variable name to trace." },
                        "session_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "read_memory",
                "description": "Read raw memory from the debuggee process. Primarily for native debugging (C/C++/Rust). Returns hex-encoded bytes. Requires adapter support (supportsReadMemoryRequest).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "memory_reference": { "type": "string", "description": "Memory reference (hex address or expression). E.g. '0x7fff5fbff8a0' or a variable's memoryReference." },
                        "offset": { "type": "integer", "description": "Byte offset from the reference. Default: 0." },
                        "count": { "type": "integer", "description": "Number of bytes to read. Default: 128." }
                    },
                    "required": ["memory_reference"]
                }
            },
            {
                "name": "write_memory",
                "description": "Write raw bytes to the debuggee's memory. Use with extreme caution. Requires adapter support (supportsWriteMemoryRequest).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "memory_reference": { "type": "string", "description": "Memory address to write to." },
                        "offset": { "type": "integer", "description": "Byte offset from the reference. Default: 0." },
                        "data": { "type": "string", "description": "Base64-encoded bytes to write." }
                    },
                    "required": ["memory_reference", "data"]
                }
            },
            {
                "name": "disassemble",
                "description": "Disassemble machine instructions at a memory address. Primarily for native debugging. Requires adapter support (supportsDisassembleRequest).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "memory_reference": { "type": "string", "description": "Memory address to start disassembly." },
                        "offset": { "type": "integer", "description": "Byte offset. Default: 0." },
                        "instruction_count": { "type": "integer", "description": "Number of instructions to disassemble. Default: 20." }
                    },
                    "required": ["memory_reference"]
                }
            },
            {
                "name": "launch_session",
                "description": "Launch a new debug session autonomously. Spawns the debug adapter, connects, sets breakpoints, and waits until the session is paused (or running). Returns the session_id for use with all other tools.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "program": { "type": "string", "description": "Absolute path to the program to debug." },
                        "adapter": { "type": "string", "description": "Debug adapter: 'python', 'node', 'typescript', 'lldb'. Default: 'python'." },
                        "config": { "type": "string", "description": "Path to a dap.json config file (alternative to adapter)." },
                        "breakpoints": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Initial breakpoints as 'file:line' strings, e.g. ['/path/app.py:42']."
                        },
                        "session_id": { "type": "string", "description": "Custom session ID. Auto-generated if omitted." }
                    },
                    "required": ["program"]
                }
            },
            {
                "name": "stop_session",
                "description": "Stop and clean up a debug session. Sends disconnect to the adapter, kills the adapter process, and removes the session from the registry.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session to stop. Defaults to 'default'." }
                    },
                    "required": []
                }
            },
            {
                "name": "wait_for_output",
                "description": "Wait until the program prints a line matching a regex pattern (or timeout). Pass from_line=console_line_count from continue_execution to only match output printed after the resume.",
                "inputSchema": {
                    "type": "object",
                    "required": ["pattern"],
                    "properties": {
                        "pattern":      { "type": "string" },
                        "timeout_secs": { "type": "integer", "description": "Max seconds to wait. Default 10." },
                        "from_line":    { "type": "integer", "description": "Skip first N console lines. Use console_line_count from continue_execution to avoid stale matches." },
                        "session_id":   { "type": "string" }
                    }
                }
            },
            {
                "name": "compare_snapshots",
                "description": "Diff the variable snapshots between two timeline stops. Returns added/removed/changed variables — no new DAP calls needed. Use get_timeline first to find stop IDs.",
                "inputSchema": {
                    "type": "object",
                    "required": ["stop_a", "stop_b"],
                    "properties": {
                        "stop_a":     { "type": "integer", "description": "Timeline entry ID of the earlier stop." },
                        "stop_b":     { "type": "integer", "description": "Timeline entry ID of the later stop." },
                        "session_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "find_first_change",
                "description": "Scan the timeline oldest-first and return the stop where a variable first changed. If expected_value is given, returns the first stop where the value differs from expected_value; otherwise returns the first stop where it differs from the initial (entry 0) value.",
                "inputSchema": {
                    "type": "object",
                    "required": ["variable_name"],
                    "properties": {
                        "variable_name":  { "type": "string", "description": "Name of the variable to track." },
                        "expected_value": { "type": "string", "description": "If set, return first stop where value != expected_value. If omitted, return first stop where value differs from the initial stop." },
                        "session_id":     { "type": "string" }
                    }
                }
            },
            {
                "name": "get_call_tree",
                "description": "Return the current call stack with locals for each frame — eliminates the need for get_stack_trace + N×(get_scopes + get_variables) round-trips. Useful for isolating bugs that span multiple frames.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "max_depth":  { "type": "integer", "description": "Maximum number of frames to inspect (default 5)." },
                        "thread_id":  { "type": "integer", "description": "Thread ID (default: paused thread)." },
                        "session_id": { "type": "string" }
                    },
                    "required": []
                }
            },
            {
                "name": "step_until_change",
                "description": "Step over instructions until a named variable changes value (or max_steps is reached). Eliminates manual step→evaluate→compare loops.",
                "inputSchema": {
                    "type": "object",
                    "required": ["variable_name"],
                    "properties": {
                        "variable_name": { "type": "string", "description": "Variable to watch for changes." },
                        "max_steps":     { "type": "integer", "description": "Maximum step_over calls before giving up (default 20)." },
                        "thread_id":     { "type": "integer", "description": "Thread ID (default 1)." },
                        "session_id":    { "type": "string" }
                    }
                }
            }
    ]);

    if !has_session {
        return json!({ "tools": all_tools });
    }

    let filtered: Vec<Value> = all_tools.as_array().unwrap().iter().filter(|tool| {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
        for &(gated_name, cap_key) in CAPABILITY_GATED_TOOLS {
            if name == gated_name {
                return adapter_caps.get(cap_key).and_then(Value::as_bool).unwrap_or(false);
            }
        }
        true
    }).cloned().collect();

    json!({ "tools": filtered })
}

// ─── Client capability tracking ──────────────────────────────────────────────

#[derive(Default, Clone)]
struct ClientCapabilities {
    elicitation_form: bool,
    elicitation_url: bool,
}

impl ClientCapabilities {
    fn from_init_params(params: &Value) -> Self {
        let caps = params.get("capabilities").unwrap_or(&Value::Null);
        let elicit = caps.get("elicitation");
        let (mut form, mut url) = (false, false);
        if let Some(e) = elicit {
            if e.is_object() {
                // Be strict: only enable when the client explicitly declares support.
                form = e.get("form").is_some();
                url = e.get("url").is_some();
            }
        }
        Self { elicitation_form: form, elicitation_url: url }
    }
}

// ─── Server-to-client request channel (for elicitation) ─────────────────────

type PendingResponses = Arc<tokio::sync::Mutex<
    std::collections::HashMap<u64, oneshot::Sender<Value>>,
>>;

static NEXT_SERVER_REQ_ID: AtomicU64 = AtomicU64::new(1_000_000);

/// Shared context threaded through tool dispatch — holds capabilities +
/// the channel needed to send server→client requests (elicitation).
#[derive(Clone)]
struct McpContext {
    client_caps: Arc<tokio::sync::RwLock<ClientCapabilities>>,
    outbox: tokio::sync::mpsc::Sender<String>,
    pending: PendingResponses,
}

impl McpContext {
    /// Send an elicitation/form request to the client.
    /// Returns `None` immediately if the client did not declare `elicitation.form`.
    #[allow(dead_code)]
    async fn elicit_form(
        &self,
        message: &str,
        schema: Value,
    ) -> Option<Value> {
        if !self.client_caps.read().await.elicitation_form {
            return None;
        }
        let req_id = NEXT_SERVER_REQ_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id, tx);

        let req = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "elicitation/create",
            "params": {
                "message": message,
                "requestedSchema": schema,
            }
        });
        if let Ok(mut s) = serde_json::to_string(&req) {
            s.push('\n');
            let _ = self.outbox.send(s).await;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(v)) => Some(v),
            _ => {
                self.pending.lock().await.remove(&req_id);
                None
            }
        }
    }

    /// Send an elicitation/url request (open a URL in the client).
    /// Returns `None` if the client did not declare `elicitation.url`.
    #[allow(dead_code)]
    async fn elicit_url(
        &self,
        message: &str,
        url: &str,
    ) -> Option<Value> {
        if !self.client_caps.read().await.elicitation_url {
            return None;
        }
        let req_id = NEXT_SERVER_REQ_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id, tx);

        let req = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "elicitation/createUrl",
            "params": {
                "message": message,
                "url": url,
            }
        });
        if let Ok(mut s) = serde_json::to_string(&req) {
            s.push('\n');
            let _ = self.outbox.send(s).await;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(v)) => Some(v),
            _ => {
                self.pending.lock().await.remove(&req_id);
                None
            }
        }
    }
}

// ─── MCP server entry point ───────────────────────────────────────────────────

pub async fn serve(registry: Arc<SessionRegistry>, hub: Arc<Hub>, proxy_port: Option<u16>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let client_caps = Arc::new(tokio::sync::RwLock::new(ClientCapabilities::default()));
    let pending: PendingResponses = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let (outbox_tx, mut outbox_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Writer task: multiplexes normal responses + server-initiated requests onto stdout
    let write_handle = tokio::spawn(async move {
        while let Some(msg) = outbox_rx.recv().await {
            debug!("[MCP OUT async] {}", msg.trim());
            if stdout.write_all(msg.as_bytes()).await.is_err() { break; }
            if stdout.flush().await.is_err() { break; }
        }
    });

    let mcp_ctx = McpContext {
        client_caps: client_caps.clone(),
        outbox: outbox_tx.clone(),
        pending: pending.clone(),
    };

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF — host disconnected
        }

        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        debug!("[MCP IN] {trimmed}");

        // Incoming messages may be client→server requests OR responses to
        // our server→client requests (elicitation). Distinguish by presence
        // of "method" field: responses have "result"/"error" but no "method".
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let resp = RpcResponse::err(None, -32700, format!("Parse error: {e}"));
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                let _ = outbox_tx.send(out).await;
                continue;
            }
        };

        if parsed.get("method").is_none() {
            // This is a response to a server-initiated request (elicitation).
            if let Some(id) = parsed.get("id").and_then(Value::as_u64) {
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let result = parsed.get("result").cloned().unwrap_or(Value::Null);
                    let _ = tx.send(result);
                }
            }
            continue;
        }

        let req: RpcRequest = match serde_json::from_value(parsed) {
            Ok(r) => r,
            Err(e) => {
                let resp = RpcResponse::err(None, -32700, format!("Parse error: {e}"));
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                let _ = outbox_tx.send(out).await;
                continue;
            }
        };

        let notification = req.id.is_none() && req.method.starts_with("notifications/");
        let response = handle_request(req, &registry, &hub, proxy_port, &client_caps, &mcp_ctx).await;

        if notification { continue; }

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        debug!("[MCP OUT] {out}");
        let _ = outbox_tx.send(out).await;
    }

    // Ensure all sender clones are dropped so the writer task can exit cleanly.
    drop(mcp_ctx);
    drop(outbox_tx);
    let _ = write_handle.await;
    Ok(())
}

// ─── Request router ───────────────────────────────────────────────────────────

async fn handle_request(
    req: RpcRequest,
    registry: &Arc<SessionRegistry>,
    hub: &Arc<Hub>,
    proxy_port: Option<u16>,
    client_caps: &Arc<tokio::sync::RwLock<ClientCapabilities>>,
    _mcp_ctx: &McpContext,
) -> RpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        // MCP lifecycle
        "initialize" => {
            if let Some(params) = &req.params {
                *client_caps.write().await = ClientCapabilities::from_init_params(params);
            }
            let caps = client_caps.read().await;
            eprintln!("[Debugium MCP] Client elicitation: form={} url={}", caps.elicitation_form, caps.elicitation_url);
            drop(caps);

            if let Some(port) = proxy_port {
                eprintln!("[Debugium MCP] Proxy mode → http://127.0.0.1:{port}");
            }
            let ids = registry.list().await;
            if ids.is_empty() && proxy_port.is_none() {
                eprintln!("[Debugium MCP] No active sessions. Use POST /sessions or launch with --mcp to start one.");
            } else if !ids.is_empty() {
                eprintln!("[Debugium MCP] Active sessions:");
                for sid in &ids {
                    if let Some(s) = registry.get(sid).await {
                        let meta = s.meta.read().await;
                        if let Some(m) = meta.as_ref() {
                            eprintln!("  session_id={sid}  program={}  adapter={}", m.program.display(), m.adapter_id);
                        } else {
                            eprintln!("  session_id={sid}  (initializing)");
                        }
                    }
                }
            }
            RpcResponse::ok(id, json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "debugium-mcp", "version": "0.2.0" }
            }))
        }
        "notifications/initialized" => {
            return RpcResponse::ok(None, json!(null));
        }
        "ping" => RpcResponse::ok(id, json!({})),

        // Tool discovery — filter by adapter capabilities when a session exists
        "tools/list" => {
            let adapter_caps = if let Some(port) = proxy_port {
                // Proxy mode: ask the running server for capabilities
                match proxy_tool_via_http(port, "get_capabilities", json!({})).await {
                    Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(json!({})),
                    Err(_) => json!({}),
                }
            } else {
                let ids = registry.list().await;
                if let Some(sid) = ids.first() {
                    if let Some(s) = registry.get(sid).await {
                        s.get_capabilities().await
                    } else { json!({}) }
                } else { json!({}) }
            };
            RpcResponse::ok(id, tool_list(&adapter_caps))
        }

        // Tool invocation
        "tools/call" => {
            let params = req.params.unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);

            // In proxy mode, always forward to the running server
            if let Some(port) = proxy_port {
                match proxy_tool_via_http(port, &name, args).await {
                    Ok(content) => return RpcResponse::ok(id, json!({ "content": [{ "type": "text", "text": content }] })),
                    Err(e) => return RpcResponse::err(id, -32603, e.to_string()),
                }
            }

            // Local dispatch (launch --mcp mode)
            match dispatch_tool(&name, args, registry, hub).await {
                Ok(content) => RpcResponse::ok(id, json!({ "content": [{ "type": "text", "text": content }] })),
                Err(e) => RpcResponse::err(id, -32603, e.to_string()),
            }
        }

        _ => RpcResponse::err(id, -32601, format!("Method not found: {}", req.method)),
    }
}

// ─── HTTP proxy for standalone MCP mode ──────────────────────────────────────

async fn proxy_tool_via_http(
    port: u16,
    tool: &str,
    args: Value,
) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/mcp-proxy"))
        .json(&json!({ "tool": tool, "args": args }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot reach Debugium server at port {port}: {e}"))?;
    let body: Value = resp.json().await
        .map_err(|e| anyhow::anyhow!("Invalid response from server: {e}"))?;
    if body["ok"].as_bool() == Some(true) {
        Ok(body["result"].as_str().unwrap_or("{}").to_string())
    } else {
        Err(anyhow::anyhow!("{}", body["error"].as_str().unwrap_or("proxy error")))
    }
}

// ─── Broadcast breakpoints_changed so the UI can update gutter dots ──────────

async fn broadcast_breakpoints_changed(hub: &Arc<Hub>, session_id: &str, file: &str, specs: &[crate::dap::session::BpSpec]) {
    use dap_types::WsEnvelope;
    let lines: Vec<u32> = specs.iter().map(|s| s.line).collect();
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "breakpoints_changed",
            "body": { "file": file, "breakpoints": lines, "specs": specs }
        }),
    };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

// ─── Broadcast a synthetic commandSent event so the web UI can animate it ────

async fn broadcast_command(hub: &Arc<Hub>, session_id: &str, command: &str) {
    use dap_types::WsEnvelope;
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "commandSent",
            "body": { "command": command, "origin": "mcp" }
        }),
    };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

// ─── Broadcast a synthetic llmQuery event so the UI shows LLM read access ────

async fn broadcast_llm_query(hub: &Arc<Hub>, session_id: &str, tool: &str, detail: &str) {
    use dap_types::WsEnvelope;
    let envelope = WsEnvelope {
        session_id: session_id.to_string(),
        msg: json!({
            "type": "event",
            "event": "llmQuery",
            "body": { "tool": tool, "detail": detail }
        }),
    };
    if let Ok(json) = serde_json::to_string(&envelope) {
        hub.broadcast(session_id, json).await;
    }
}

// ─── Tool dispatcher ─────────────────────────────────────────────────────────

pub async fn dispatch_tool(
    name: &str,
    args: Value,
    registry: &Arc<SessionRegistry>,
    hub: &Arc<Hub>,
) -> Result<String> {
    let session_id = args.get("session_id").and_then(Value::as_str).unwrap_or("default");
    let session = registry.get(session_id).await;

    // Broadcast every tool call to the UI so the human sees AI activity in real time.
    // Individual tools may broadcast additional detail (matched output, etc).
    broadcast_llm_query(hub, session_id, name, "").await;

    match name {
        // ── Session ────────────────────────────────────────────────────
        "get_sessions" => {
            let ids = registry.list().await;
            Ok(serde_json::to_string_pretty(&json!({ "sessions": ids }))?)
        }

        // ── Breakpoints ────────────────────────────────────────────────
        "set_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let lines: Vec<u32> = args.get("lines").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("`lines` is required"))?
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect();
            let resp = s.set_breakpoints(file, lines.clone()).await?;
            // Broadcast breakpoints_changed so the UI updates immediately
            let verified = s.breakpoints.read().await
                .get(file).cloned().unwrap_or_default();
            broadcast_breakpoints_changed(hub, session_id, file, &verified).await;
            Ok(format!("Breakpoints set in {file} at lines {:?}\n{}", lines,
                serde_json::to_string_pretty(&resp)?))
        }

        "list_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let bps = s.breakpoints.read().await.clone();
            Ok(serde_json::to_string_pretty(&json!({ "breakpoints": bps }))?)
        }

        "clear_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let files: Vec<String> = s.breakpoints.read().await.keys().cloned().collect();
            for file in &files {
                s.set_breakpoints(file, vec![]).await?;
                broadcast_breakpoints_changed(hub, session_id, file, &[] as &[crate::dap::session::BpSpec]).await;
            }
            Ok(format!("Cleared breakpoints in {} file(s): {:?}", files.len(), files))
        }

        // ── Execution control ──────────────────────────────────────────
        "continue_execution" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            broadcast_command(hub, session_id, "continue").await;
            // Capture line count BEFORE continuing so the LLM can pass it to wait_for_output.
            let console_line_count = s.console_lines.read().await.len();
            s.client.request("continue", Some(json!({ "threadId": thread_id }))).await?;
            Ok(serde_json::to_string_pretty(&json!({
                "status": "running",
                "console_line_count": console_line_count,
                "hint": "Pass console_line_count as from_line to wait_for_output to only match new output."
            }))?)
        }

        "step_over" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "next").await;
            let mut stopped_rx = s.stopped_tx.subscribe();
            s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
            let loc = await_stopped(&s, &mut stopped_rx).await;
            Ok(format!("Stepped over → {loc}"))
        }

        "step_in" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "stepIn").await;
            let mut stopped_rx = s.stopped_tx.subscribe();
            s.client.request("stepIn", Some(json!({ "threadId": thread_id }))).await?;
            let loc = await_stopped(&s, &mut stopped_rx).await;
            Ok(format!("Stepped in → {loc}"))
        }

        "step_out" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "stepOut").await;
            let mut stopped_rx = s.stopped_tx.subscribe();
            s.client.request("stepOut", Some(json!({ "threadId": thread_id }))).await?;
            let loc = await_stopped(&s, &mut stopped_rx).await;
            Ok(format!("Stepped out → {loc}"))
        }

        "pause" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            broadcast_command(hub, session_id, "pause").await;
            let resp = s.client.request("pause", Some(json!({ "threadId": thread_id }))).await?;
            Ok(format!("Paused thread {thread_id}\n{}", serde_json::to_string_pretty(&resp)?))
        }

        // ── Inspection ─────────────────────────────────────────────────
        "get_threads" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            broadcast_command(hub, session_id, "threads").await;
            let resp = s.client.request("threads", None).await?;
            let threads = resp.get("body").and_then(|b| b.get("threads")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&threads)?)
        }

        "get_stack_trace" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };
            let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(20);
            broadcast_command(hub, session_id, "stackTrace").await;
            let resp = s.client.request("stackTrace", Some(json!({
                "threadId": thread_id, "startFrame": 0, "levels": depth
            }))).await?;
            let frames = resp.get("body").and_then(|b| b.get("stackFrames")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&frames)?)
        }

        "get_scopes" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let frame_id = require_u32(&args, "frame_id")?;
            broadcast_command(hub, session_id, "scopes").await;
            let resp = s.client.request("scopes", Some(json!({ "frameId": frame_id }))).await?;
            let scopes = resp.get("body").and_then(|b| b.get("scopes")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&scopes)?)
        }

        "get_variables" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let vref = require_u32(&args, "variables_reference")?;
            broadcast_command(hub, session_id, "variables").await;
            let resp = s.client.request("variables", Some(json!({ "variablesReference": vref }))).await?;
            let vars = resp.get("body").and_then(|b| b.get("variables")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&vars)?)
        }

        "evaluate" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let expr = args.get("expression").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`expression` is required"))?;
            // frame_id is optional; auto-resolve top frame when omitted
            let frame_id = if let Some(v) = args.get("frame_id").and_then(Value::as_u64) {
                v as u32
            } else {
                let tid = paused_thread_id(&s).await;
                let st = s.client.request("stackTrace", Some(json!({
                    "threadId": tid, "startFrame": 0, "levels": 1
                }))).await?;
                st.get("body").and_then(|b| b["stackFrames"].as_array())
                    .and_then(|a| a.first())
                    .and_then(|f| f["id"].as_u64())
                    .unwrap_or(1) as u32
            };
            let context = args.get("context").and_then(Value::as_str).unwrap_or("repl");
            let resp = s.client.request("evaluate", Some(json!({
                "expression": expr,
                "frameId": frame_id,
                "context": context
            }))).await?;
            let result = resp.get("body").and_then(|b| b.get("result")).cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&result)?)
        }

        // ── Source ─────────────────────────────────────────────────────
        "get_source" => {
            let path = args.get("path").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`path` is required"))?;
            let content = tokio::fs::read_to_string(path).await
                .map_err(|e| anyhow::anyhow!("Could not read {path}: {e}"))?;
            let lines: Vec<&str> = content.lines().collect();

            if let Some(center) = args.get("around_line").and_then(Value::as_u64).map(|n| n as usize) {
                let ctx = args.get("context_lines").and_then(Value::as_u64).unwrap_or(10) as usize;
                let start = center.saturating_sub(ctx + 1);
                let end = (center + ctx).min(lines.len());
                let snippet: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        let n = start + i + 1;
                        let marker = if n == center { "→" } else { " " };
                        format!("{marker} {n:4}: {l}")
                    })
                    .collect();
                Ok(snippet.join("\n"))
            } else {
                let numbered: Vec<String> = lines.iter().enumerate()
                    .map(|(i, l)| format!("{:4}: {l}", i + 1))
                    .collect();
                Ok(numbered.join("\n"))
            }
        }

        // ── Lifecycle ──────────────────────────────────────────────────
        "terminate" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let resp = s.client.request("terminate", Some(json!({ "restart": false }))).await?;
            Ok(format!("Terminate requested.\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "restart" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            s.client.notify("restart", None).await?;
            Ok("Restart requested.".to_string())
        }

        "set_exception_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let filters: Vec<String> = args.get("filters").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("`filters` is required"))?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            let resp = s.client.request("setExceptionBreakpoints", Some(json!({ "filters": filters }))).await?;
            Ok(format!("Exception breakpoints set: {:?}\n{}", filters, serde_json::to_string_pretty(&resp)?))
        }

        "get_capabilities" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let caps = s.get_capabilities().await;
            Ok(serde_json::to_string_pretty(&caps)?)
        }

        "get_exception_info" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let thread_id = require_u32(&args, "thread_id")?;
            let resp = s.client.request("exceptionInfo", Some(json!({ "threadId": thread_id }))).await?;
            let body = resp.get("body").cloned().unwrap_or(Value::Null);
            Ok(serde_json::to_string_pretty(&body)?)
        }

        "set_variable" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let vref = require_u32(&args, "variables_reference")?;
            let name = args.get("name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`name` is required"))?;
            let value = args.get("value").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`value` is required"))?;
            let resp = s.client.request("setVariable", Some(json!({
                "variablesReference": vref,
                "name": name,
                "value": value
            }))).await?;
            Ok(format!("Variable '{name}' set to '{value}'\n{}", serde_json::to_string_pretty(&resp)?))
        }

        "set_function_breakpoints" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session not found"))?;
            let names: Vec<&str> = args.get("names").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("`names` is required"))?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let bps: Vec<Value> = names.iter().map(|n| json!({ "name": n })).collect();
            let resp = s.client.request("setFunctionBreakpoints", Some(json!({ "breakpoints": bps }))).await?;
            Ok(format!("Function breakpoints set: {:?}\n{}", names, serde_json::to_string_pretty(&resp)?))
        }

        // ── Compound / LLM-optimised tools ────────────────────────────
        "get_debug_context" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };
            let compact = args.get("verbosity").and_then(Value::as_str).unwrap_or("full") == "compact";
            let max_frames: usize = if compact { 3 } else { 20 };
            let max_vars: usize = if compact { 10 } else { 30 };

            // 1. stack trace
            let stack_resp = s.client.request("stackTrace", Some(json!({
                "threadId": thread_id, "startFrame": 0, "levels": max_frames
            }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();
            let top = frames.first();
            let frame_id = top.and_then(|f| f.get("id")).and_then(Value::as_u64).unwrap_or(1) as u32;
            let file = top.and_then(|f| f.get("source")).and_then(|s| s.get("path"))
                .and_then(Value::as_str).unwrap_or("?").to_string();
            let line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let func = top.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("?").to_string();
            let stack_summary: Vec<String> = frames.iter().map(|f| {
                let fname = f.get("name").and_then(Value::as_str).unwrap_or("?");
                let ffile = f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str)
                    .map(|p| p.split('/').last().unwrap_or(p)).unwrap_or("?");
                let fline = f.get("line").and_then(Value::as_u64).unwrap_or(0);
                format!("{}:{} in {}()", ffile, fline, fname)
            }).collect();

            // 2. scopes → locals
            let mut locals = serde_json::Map::new();
            if let Ok(scopes_resp) = s.client.request("scopes", Some(json!({ "frameId": frame_id }))).await {
                let scopes = scopes_resp.get("body").and_then(|b| b.get("scopes"))
                    .and_then(Value::as_array).cloned().unwrap_or_default();
                let locals_scope = scopes.iter().find(|sc| {
                    sc.get("name").and_then(Value::as_str)
                        .map(|n| n.to_lowercase().contains("local") || n.to_lowercase().contains("function"))
                        .unwrap_or(false)
                }).or_else(|| scopes.first());
                if let Some(sc) = locals_scope {
                    if let Some(vref) = sc.get("variablesReference").and_then(Value::as_u64) {
                        if vref > 0 {
                            if let Ok(vars_resp) = s.client.request("variables",
                                Some(json!({ "variablesReference": vref }))).await {
                                if let Some(vars) = vars_resp.get("body").and_then(|b| b.get("variables"))
                                    .and_then(Value::as_array) {
                                    for v in vars.iter().take(max_vars) {
                                        let name = v.get("name").and_then(Value::as_str).unwrap_or("?");
                                        let val  = v.get("value").and_then(Value::as_str).unwrap_or("?");
                                        let typ  = v.get("type").and_then(Value::as_str).unwrap_or("");
                                        let entry = if typ.is_empty() { val.to_string() }
                                                    else { format!("{val} ({typ})") };
                                        locals.insert(name.to_string(), json!(entry));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 3. source window ±5 lines
            let source_window = if let Ok(content) = tokio::fs::read_to_string(&file).await {
                let lines: Vec<&str> = content.lines().collect();
                let ctx = 5usize;
                let start = (line as usize).saturating_sub(ctx + 1);
                let end = ((line as usize) + ctx).min(lines.len());
                lines[start..end].iter().enumerate()
                    .map(|(i, l)| {
                        let n = start + i + 1;
                        let marker = if n == line as usize { "→" } else { " " };
                        format!("{marker} {:4}: {l}", n)
                    })
                    .collect::<Vec<_>>().join("\n")
            } else { String::new() };

            let bps = s.breakpoints.read().await.clone();
            let result = json!({
                "paused_at": format!("{}:{} in {}()",
                    file.split('/').last().unwrap_or(&file), line, func),
                "file": file,
                "line": line,
                "function": func,
                "frame_id": frame_id,
                "thread_id": thread_id,
                "locals": locals,
                "call_stack": stack_summary,
                "source_window": source_window,
                "breakpoints": bps,
            });
            Ok(serde_json::to_string_pretty(&result)?)
        }

        "annotate" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let line = require_u32(&args, "line")?;
            let message = args.get("message").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`message` is required"))?;
            let color = args.get("color").and_then(Value::as_str).unwrap_or("warning");
            let ann = s.add_annotation(file.to_string(), line, message.to_string(), color.to_string()).await;
            // Broadcast so the UI updates immediately
            use dap_types::WsEnvelope;
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event", "event": "annotation_added",
                    "body": { "id": ann.id, "file": ann.file, "line": ann.line,
                              "message": ann.message, "color": ann.color }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("[{}] {}:{} — {}", color, file.split('/').last().unwrap_or(file), line, message))
        }

        "add_finding" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let message = args.get("message").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`message` is required"))?;
            let level = args.get("level").and_then(Value::as_str).unwrap_or("info");
            let f = s.add_finding(message.to_string(), level.to_string()).await;
            use dap_types::WsEnvelope;
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event", "event": "finding_added",
                    "body": { "id": f.id, "message": f.message, "level": f.level, "timestamp": f.timestamp }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("[{}] {}", level, message))
        }

        "step_until" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let condition = args.get("condition").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`condition` is required"))?;
            let max_steps = args.get("max_steps").and_then(Value::as_u64).unwrap_or(20) as usize;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;

            let mut steps = 0;
            let mut hit = false;
            while steps < max_steps {
                s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
                s.wait_for_stop(15).await?;
                steps += 1;

                // Get top frame id for evaluate
                let frame_id = s.client.request("stackTrace",
                    Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }))).await
                    .ok()
                    .and_then(|r| r.get("body")?.get("stackFrames")?.as_array()?.first().cloned())
                    .and_then(|f| f.get("id")?.as_u64())
                    .unwrap_or(1) as u32;

                let eval = s.client.request("evaluate", Some(json!({
                    "expression": condition,
                    "frameId": frame_id,
                    "context": "watch"
                }))).await;
                let result = eval.ok()
                    .and_then(|r| r.get("body")?.get("result")?.as_str().map(str::to_string))
                    .unwrap_or_else(|| "False".to_string());
                if result != "False" && result != "false" && result != "0" && result != "None" {
                    hit = true;
                    break;
                }
            }
            let status = if hit { format!("Condition met after {steps} step(s)") }
                         else { format!("Reached max_steps={max_steps} without condition firing") };
            Ok(status)
        }

        "run_until_exception" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;
            let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(30);

            // Enable 'raised' exception breakpoints
            if let Ok(caps) = s.get_capabilities().await.get("exceptionBreakpointFilters")
                .and_then(Value::as_array).map(|a| !a.is_empty()).ok_or(()) {
                if caps {
                    s.client.request("setExceptionBreakpoints",
                        Some(json!({ "filters": ["raised"] }))).await.ok();
                }
            } else {
                s.client.request("setExceptionBreakpoints",
                    Some(json!({ "filters": ["raised"] }))).await.ok();
            }

            // Wait briefly for any in-flight stop event (handles race where exception
            // fires just as this tool is called and last_stopped isn't set yet)
            if s.last_stopped.read().await.is_none() {
                // Give up to 5s for a pending stop event before deciding to continue
                let _ = s.wait_for_stop(5).await;
            }
            // If already stopped at an exception, use current state; otherwise continue and wait
            let already_at_exception = s.last_stopped.read().await
                .as_ref()
                .and_then(|ev| ev.get("body"))
                .and_then(|b| b.get("reason"))
                .and_then(Value::as_str)
                .map(|r| r == "exception")
                .unwrap_or(false);
            if !already_at_exception {
                s.client.request("continue", Some(json!({ "threadId": thread_id }))).await?;
                s.wait_for_stop(timeout).await?;
            }

            // Reuse get_debug_context logic: call recursively via a fake args
            let ctx_args = json!({ "session_id": session_id, "thread_id": thread_id });
            let ctx_name = "get_debug_context";
            // Inline: get stack + scopes + vars
            let stack_resp = s.client.request("stackTrace",
                Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 10 }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();
            let top = frames.first();
            let frame_id = top.and_then(|f| f.get("id")).and_then(Value::as_u64).unwrap_or(1) as u32;
            let file = top.and_then(|f| f.get("source")).and_then(|s| s.get("path"))
                .and_then(Value::as_str).unwrap_or("?").to_string();
            let line = top.and_then(|f| f.get("line")).and_then(Value::as_u64).unwrap_or(0) as u32;
            let func = top.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("?").to_string();

            // Exception info
            let exc_info = s.client.request("exceptionInfo",
                Some(json!({ "threadId": thread_id }))).await
                .ok()
                .and_then(|r| r.get("body").cloned())
                .unwrap_or(json!(null));

            let _ = ctx_args; let _ = ctx_name;
            Ok(serde_json::to_string_pretty(&json!({
                "exception": exc_info,
                "paused_at": format!("{}:{} in {}()",
                    file.split('/').last().unwrap_or(&file), line, func),
                "file": file, "line": line, "function": func, "frame_id": frame_id,
                "call_stack": frames.iter().map(|f| {
                    format!("{}:{} in {}()",
                        f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str)
                            .map(|p| p.split('/').last().unwrap_or(p)).unwrap_or("?"),
                        f.get("line").and_then(Value::as_u64).unwrap_or(0),
                        f.get("name").and_then(Value::as_str).unwrap_or("?"))
                }).collect::<Vec<_>>(),
            }))?)
        }

        "disconnect" => {
            let terminate = args.get("terminate_debuggee").and_then(Value::as_bool).unwrap_or(true);
            if let Some(s) = session {
                let resp = s.client.request("disconnect", Some(json!({ "terminateDebuggee": terminate }))).await?;
                Ok(format!("Disconnected.\n{}", serde_json::to_string_pretty(&resp)?))
            } else {
                Ok("No active session to disconnect.".to_string())
            }
        }

        "set_breakpoint" => {
            use crate::dap::session::BpSpec;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let file = args.get("file").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            let line = require_u32(&args, "line")?;
            let condition = args.get("condition").and_then(Value::as_str).map(str::to_string);

            // Build specs list preserving existing ones, add/replace this line
            let mut existing: Vec<BpSpec> = s.breakpoints.read().await
                .get(file).cloned().unwrap_or_default();
            if let Some(pos) = existing.iter().position(|s| s.line == line) {
                existing[pos].condition = condition.clone();
            } else {
                existing.push(BpSpec { line, condition: condition.clone() });
            }
            let resp = s.set_breakpoints_with_conditions(file, existing).await?;
            let verified = resp.get("body").and_then(|b| b.get("breakpoints"))
                .and_then(Value::as_array)
                .and_then(|arr| arr.iter().find(|bp| {
                    bp.get("line").and_then(Value::as_u64).unwrap_or(0) == line as u64
                }))
                .and_then(|bp| bp.get("line").and_then(Value::as_u64))
                .unwrap_or(line as u64) as u32;
            let stored = s.breakpoints.read().await.get(file).cloned().unwrap_or_default();
            broadcast_breakpoints_changed(hub, session_id, file, &stored).await;
            let cond_msg = condition.map(|c| format!(" [condition: {c}]")).unwrap_or_default();
            Ok(format!("Breakpoint set at {}:{} (verified line: {}){}", file, line, verified, cond_msg))
        }

        "get_console_output" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let n = args.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
            let buf = s.console_lines.read().await;
            let lines: Vec<&str> = buf.iter().rev().take(n).map(|s| s.as_str()).collect();
            let output: Vec<&str> = lines.into_iter().rev().collect();
            if output.is_empty() {
                Ok("(no output yet)".to_string())
            } else {
                Ok(output.join("\n"))
            }
        }

        "list_sessions" => {
            let ids = registry.list().await;
            let mut result = Vec::new();
            for sid in &ids {
                if let Some(s) = registry.get(sid).await {
                    let meta = s.meta.read().await;
                    let entry = if let Some(m) = meta.as_ref() {
                        json!({
                            "id": sid,
                            "program": m.program.display().to_string(),
                            "adapter": m.adapter_id,
                            "adapter_pid": m.adapter_pid,
                            "started_at": m.started_at,
                            "port": m.port,
                        })
                    } else {
                        json!({ "id": sid, "status": "initializing" })
                    };
                    result.push(entry);
                }
            }
            Ok(serde_json::to_string_pretty(&json!({ "sessions": result }))?)
        }

        // ── Timeline ───────────────────────────────────────────────
        "get_timeline" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            let tl = s.timeline.read().await;
            let entries: Vec<_> = tl.iter().rev().take(limit).cloned().collect();
            let entries: Vec<_> = entries.into_iter().rev().collect();
            Ok(serde_json::to_string_pretty(&json!({ "timeline": entries, "total": tl.len() }))?)
        }

        // ── Watch expressions ──────────────────────────────────────
        "add_watch" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let expr = args.get("expression").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`expression` is required"))?;
            {
                let mut watches = s.watches.write().await;
                if !watches.contains(&expr.to_string()) {
                    watches.push(expr.to_string());
                }
            }
            // Broadcast watches_updated so the UI shows it immediately
            use dap_types::WsEnvelope;
            let watches_now = s.watches.read().await.clone();
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event",
                    "event": "watches_list_changed",
                    "body": { "watches": watches_now }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("Watch added: {expr}"))
        }

        "remove_watch" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let expr = args.get("expression").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`expression` is required"))?;
            s.watches.write().await.retain(|e| e != expr);
            s.watch_results.write().await.retain(|r| r.expression != expr);
            use dap_types::WsEnvelope;
            let watches_now = s.watches.read().await.clone();
            let env = WsEnvelope {
                session_id: session_id.to_string(),
                msg: json!({
                    "type": "event",
                    "event": "watches_list_changed",
                    "body": { "watches": watches_now }
                }),
            };
            if let Ok(j) = serde_json::to_string(&env) { hub.broadcast(session_id, j).await; }
            Ok(format!("Watch removed: {expr}"))
        }

        "get_watches" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let watches = s.watches.read().await.clone();
            let results = s.watch_results.read().await.clone();
            Ok(serde_json::to_string_pretty(&json!({
                "watches": watches,
                "results": results
            }))?)
        }

        "get_annotations" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let anns = s.annotations.read().await.clone();
            let detail = format!("{} annotation{}", anns.len(), if anns.len() == 1 { "" } else { "s" });
            broadcast_llm_query(hub, session_id, "get_annotations", &detail).await;
            Ok(serde_json::to_string_pretty(&json!({ "annotations": anns }))?)
        }

        "get_findings" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let findings = s.findings.read().await.clone();
            let detail = format!("{} finding{}", findings.len(), if findings.len() == 1 { "" } else { "s" });
            broadcast_llm_query(hub, session_id, "get_findings", &detail).await;
            Ok(serde_json::to_string_pretty(&json!({ "findings": findings }))?)
        }

        "get_variable_history" => {
            let name = args["name"].as_str()
                .ok_or_else(|| anyhow::anyhow!("get_variable_history: 'name' required"))?;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            broadcast_llm_query(hub, session_id, "get_variable_history", name).await;
            let tl = s.timeline.read().await;
            let history: Vec<Value> = tl.iter()
                .filter_map(|e| {
                    e.variables_snapshot.get(name).map(|val| json!({
                        "timeline_id": e.id,
                        "file": e.file,
                        "line": e.line,
                        "timestamp": e.timestamp,
                        "value": val
                    }))
                })
                .collect();
            let result_detail = if history.is_empty() {
                format!("{name} → no history")
            } else {
                format!("{name} → {} stop{}", history.len(), if history.len() == 1 { "" } else { "s" })
            };
            broadcast_llm_query(hub, session_id, "get_variable_history", &result_detail).await;
            Ok(serde_json::to_string_pretty(&json!({ "name": name, "history": history }))?)
        }

        "wait_for_output" => {
            let pattern = args["pattern"].as_str()
                .ok_or_else(|| anyhow::anyhow!("wait_for_output: 'pattern' required"))?;
            let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(10);
            let re = regex::Regex::new(pattern)
                .map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?;
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            // from_line: skip the first N lines of the buffer — use the console_line_count from
            // continue_execution to avoid matching output printed before the current run.
            let from_line = args.get("from_line").and_then(Value::as_u64).unwrap_or(0) as usize;
            broadcast_llm_query(hub, session_id, "wait_for_output", pattern).await;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
            loop {
                {
                    let lines = s.console_lines.read().await;
                    if let Some(line) = lines.iter().skip(from_line).find(|l| re.is_match(l)) {
                        let matched_line = line.clone();
                        drop(lines);
                        let detail = format!("matched: \"{}\"", matched_line.chars().take(60).collect::<String>());
                        broadcast_llm_query(hub, session_id, "wait_for_output", &detail).await;
                        return Ok(serde_json::to_string_pretty(&json!({ "matched": true, "line": matched_line }))?);
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    broadcast_llm_query(hub, session_id, "wait_for_output", "timed out").await;
                    return Ok(serde_json::to_string_pretty(&json!({ "matched": false, "line": Value::Null }))?);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        // ── Memory inspection ────────────────────────────────────────
        "read_memory" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let caps = s.get_capabilities().await;
            if !caps.get("supportsReadMemoryRequest").and_then(Value::as_bool).unwrap_or(false) {
                return Ok("Adapter does not support readMemory (supportsReadMemoryRequest=false). This feature requires a native debugger like lldb-dap.".to_string());
            }
            let mem_ref = args.get("memory_reference").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`memory_reference` is required"))?;
            let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
            let count = args.get("count").and_then(Value::as_u64).unwrap_or(128);

            let resp = s.client.request("readMemory", Some(json!({
                "memoryReference": mem_ref,
                "offset": offset,
                "count": count,
            }))).await?;
            let body = resp.get("body").cloned().unwrap_or(json!(null));
            Ok(serde_json::to_string_pretty(&body)?)
        }

        "write_memory" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let caps = s.get_capabilities().await;
            if !caps.get("supportsWriteMemoryRequest").and_then(Value::as_bool).unwrap_or(false) {
                return Ok("Adapter does not support writeMemory (supportsWriteMemoryRequest=false).".to_string());
            }
            let mem_ref = args.get("memory_reference").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`memory_reference` is required"))?;
            let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
            let data = args.get("data").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`data` (base64) is required"))?;

            let resp = s.client.request("writeMemory", Some(json!({
                "memoryReference": mem_ref,
                "offset": offset,
                "data": data,
            }))).await?;
            let written = resp.get("body")
                .and_then(|b| b.get("bytesWritten")).and_then(Value::as_u64).unwrap_or(0);
            Ok(format!("Wrote {written} byte(s) to {mem_ref}+{offset}"))
        }

        "disassemble" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let caps = s.get_capabilities().await;
            if !caps.get("supportsDisassembleRequest").and_then(Value::as_bool).unwrap_or(false) {
                return Ok("Adapter does not support disassemble (supportsDisassembleRequest=false).".to_string());
            }
            let mem_ref = args.get("memory_reference").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`memory_reference` is required"))?;
            let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
            let count = args.get("instruction_count").and_then(Value::as_u64).unwrap_or(20);

            let resp = s.client.request("disassemble", Some(json!({
                "memoryReference": mem_ref,
                "offset": offset,
                "instructionCount": count,
            }))).await?;
            let body = resp.get("body").cloned().unwrap_or(json!(null));
            Ok(serde_json::to_string_pretty(&body)?)
        }

        // ── Session lifecycle ─────────────────────────────────────
        "launch_session" => {
            let program_str = args.get("program").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`program` is required"))?;
            let program = std::path::PathBuf::from(program_str);
            let adapter_str = args.get("adapter").and_then(Value::as_str);
            let config_path = args.get("config").and_then(Value::as_str);

            let kind = if let Some(cfg) = config_path {
                crate::dap::adapter::AdapterKind::from_str(cfg)
            } else if let Some(a) = adapter_str {
                crate::dap::adapter::AdapterKind::from_str(a)
            } else {
                // Auto-detect: try multi-config, then extension, then default to python
                crate::dap::adapter::adapter_kind_from_extension(&program)
                    .unwrap_or(crate::dap::adapter::AdapterKind::Python)
            };

            let adapter = crate::dap::adapter::Adapter::new(kind);
            let sid = args.get("session_id").and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!("session-{}", chrono::Utc::now().timestamp_millis())
                });

            let session = crate::dap::session::Session::new(sid.clone(), adapter, hub.clone())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create session: {e}"))?;

            registry.insert(session.clone()).await;

            // Auto-remove session from registry after termination
            {
                let reg = registry.clone();
                let sid_cleanup = sid.clone();
                let mut term_rx = session.terminated_tx.subscribe();
                tokio::spawn(async move {
                    while term_rx.changed().await.is_ok() {
                        if *term_rx.borrow() {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            reg.remove(&sid_cleanup).await;
                            break;
                        }
                    }
                });
            }

            // Parse breakpoints
            let bp_strs: Vec<String> = args.get("breakpoints")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let breakpoints = parse_breakpoint_strings(&bp_strs);

            let cwd = std::env::current_dir().unwrap_or_default();

            // Run configure_and_launch and wait for it to complete (includes DAP handshake)
            let session2 = session.clone();
            let program2 = program.clone();
            let cwd2 = cwd.clone();
            let launch_handle = tokio::spawn(async move {
                session2.configure_and_launch(program2, cwd2, &breakpoints).await
            });

            // Wait up to 30s for the launch to complete
            match tokio::time::timeout(std::time::Duration::from_secs(30), launch_handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => return Err(anyhow::anyhow!("Launch failed: {e}")),
                Ok(Err(e)) => return Err(anyhow::anyhow!("Launch task panicked: {e}")),
                Err(_) => return Err(anyhow::anyhow!("Launch timed out after 30s")),
            }

            // Try to wait briefly for a breakpoint hit
            let status = match session.wait_for_stop(5).await {
                Ok(()) => "paused",
                Err(_) => "running",
            };

            // Broadcast session_launched on ALL existing sessions so connected UI clients hear it
            {
                use dap_types::WsEnvelope;
                let all_ids = registry.list().await;
                for existing_id in &all_ids {
                    let envelope = WsEnvelope {
                        session_id: sid.clone(),
                        msg: json!({
                            "type": "event",
                            "event": "session_launched",
                            "body": { "session_id": sid, "program": program_str, "status": status }
                        }),
                    };
                    if let Ok(j) = serde_json::to_string(&envelope) {
                        hub.broadcast(existing_id, j).await;
                    }
                }
            }

            Ok(serde_json::to_string_pretty(&json!({
                "session_id": sid,
                "status": status,
                "program": program_str,
                "hint": "Call get_debug_context to see where execution paused."
            }))?)
        }

        "stop_session" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;

            // Send disconnect to adapter
            let _ = s.client.request("disconnect", Some(json!({ "terminateDebuggee": true }))).await;

            // Kill adapter process if we have a PID
            if let Some(meta) = s.meta.read().await.as_ref() {
                if let Some(pid) = meta.adapter_pid {
                    let _ = nix_kill(pid);
                }
            }

            // Remove from registry
            registry.remove(session_id).await;

            Ok(format!("Session '{session_id}' stopped and removed."))
        }

        // ── New compound tools ─────────────────────────────────────
        "compare_snapshots" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let stop_a = args.get("stop_a").and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("`stop_a` is required"))? as u32;
            let stop_b = args.get("stop_b").and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("`stop_b` is required"))? as u32;
            broadcast_llm_query(hub, session_id, "compare_snapshots",
                &format!("stop {} vs {}", stop_a, stop_b)).await;
            let tl = s.timeline.read().await;
            let entry_a = tl.iter().find(|e| e.id == stop_a)
                .ok_or_else(|| anyhow::anyhow!("Timeline stop {} not found", stop_a))?;
            let entry_b = tl.iter().find(|e| e.id == stop_b)
                .ok_or_else(|| anyhow::anyhow!("Timeline stop {} not found", stop_b))?;
            let snap_a = &entry_a.variables_snapshot;
            let snap_b = &entry_b.variables_snapshot;
            let mut added = serde_json::Map::new();
            let mut removed = serde_json::Map::new();
            let mut changed = serde_json::Map::new();
            let mut unchanged_count: usize = 0;
            for (k, v_b) in snap_b {
                match snap_a.get(k) {
                    None => { added.insert(k.clone(), json!(v_b)); }
                    Some(v_a) if v_a != v_b => {
                        changed.insert(k.clone(), json!({ "from": v_a, "to": v_b }));
                    }
                    _ => { unchanged_count += 1; }
                }
            }
            for k in snap_a.keys() {
                if !snap_b.contains_key(k) {
                    removed.insert(k.clone(), json!(snap_a[k]));
                }
            }
            Ok(serde_json::to_string_pretty(&json!({
                "stop_a": { "id": entry_a.id, "file": entry_a.file, "line": entry_a.line, "timestamp": entry_a.timestamp },
                "stop_b": { "id": entry_b.id, "file": entry_b.file, "line": entry_b.line, "timestamp": entry_b.timestamp },
                "added": added,
                "removed": removed,
                "changed": changed,
                "unchanged_count": unchanged_count
            }))?)
        }

        "find_first_change" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let var_name = args.get("variable_name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`variable_name` is required"))?;
            let expected = args.get("expected_value").and_then(Value::as_str).map(str::to_string);
            broadcast_llm_query(hub, session_id, "find_first_change", var_name).await;
            let tl = s.timeline.read().await;
            // Collect entries oldest-first (timeline is stored newest-last already)
            let entries: Vec<_> = tl.iter().collect();
            let baseline = if let Some(ref exp) = expected {
                exp.clone()
            } else {
                // Use the initial observed value from the first entry that has this variable
                entries.iter()
                    .find_map(|e| e.variables_snapshot.get(var_name).cloned())
                    .unwrap_or_default()
            };
            let total = entries.len();
            let first_change = entries.iter().enumerate().find_map(|(i, e)| {
                let val = e.variables_snapshot.get(var_name)?;
                // Skip the very first entry when no expected_value: that's our baseline
                if expected.is_none() && i == 0 { return None; }
                if val != &baseline {
                    let old = if i > 0 {
                        entries[i - 1].variables_snapshot.get(var_name).cloned()
                    } else {
                        None
                    };
                    Some(json!({
                        "stop_id": e.id,
                        "file": e.file,
                        "line": e.line,
                        "timestamp": e.timestamp,
                        "old": old.as_deref().unwrap_or(&baseline),
                        "new": val
                    }))
                } else {
                    None
                }
            });
            Ok(serde_json::to_string_pretty(&json!({
                "variable": var_name,
                "baseline_value": baseline,
                "first_change_at": first_change,
                "total_stops_searched": total
            }))?)
        }

        "get_call_tree" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let max_depth = args.get("max_depth").and_then(Value::as_u64).unwrap_or(5) as usize;
            let thread_id = if let Some(t) = args.get("thread_id").and_then(Value::as_u64) {
                t as u32
            } else {
                paused_thread_id(&s).await
            };
            broadcast_command(hub, session_id, "stackTrace").await;
            let stack_resp = s.client.request("stackTrace", Some(json!({
                "threadId": thread_id, "startFrame": 0, "levels": max_depth
            }))).await?;
            let frames = stack_resp.get("body").and_then(|b| b.get("stackFrames"))
                .and_then(Value::as_array).cloned().unwrap_or_default();

            let mut call_tree: Vec<Value> = Vec::new();
            for (depth, frame) in frames.iter().take(max_depth).enumerate() {
                let frame_id = frame.get("id").and_then(Value::as_u64).unwrap_or(1) as u32;
                let file = frame.get("source").and_then(|s| s.get("path"))
                    .and_then(Value::as_str).unwrap_or("?").to_string();
                let line = frame.get("line").and_then(Value::as_u64).unwrap_or(0);
                let func = frame.get("name").and_then(Value::as_str).unwrap_or("?").to_string();

                // Fetch locals for this frame
                let mut locals = serde_json::Map::new();
                if let Ok(scopes_resp) = s.client.request("scopes",
                    Some(json!({ "frameId": frame_id }))).await {
                    let scopes = scopes_resp.get("body").and_then(|b| b.get("scopes"))
                        .and_then(Value::as_array).cloned().unwrap_or_default();
                    let locals_scope = scopes.iter().find(|sc| {
                        sc.get("name").and_then(Value::as_str)
                            .map(|n| n.to_lowercase().contains("local") || n.to_lowercase().contains("function"))
                            .unwrap_or(false)
                    }).or_else(|| scopes.first());
                    if let Some(sc) = locals_scope {
                        if let Some(vref) = sc.get("variablesReference").and_then(Value::as_u64) {
                            if vref > 0 {
                                if let Ok(vars_resp) = s.client.request("variables",
                                    Some(json!({ "variablesReference": vref }))).await {
                                    if let Some(vars) = vars_resp.get("body")
                                        .and_then(|b| b.get("variables"))
                                        .and_then(Value::as_array) {
                                        for v in vars.iter().take(20) {
                                            let vname = v.get("name").and_then(Value::as_str).unwrap_or("?");
                                            let val   = v.get("value").and_then(Value::as_str).unwrap_or("?");
                                            let typ   = v.get("type").and_then(Value::as_str).unwrap_or("");
                                            let entry = if typ.is_empty() { val.to_string() }
                                                        else { format!("{val} ({typ})") };
                                            locals.insert(vname.to_string(), json!(entry));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                call_tree.push(json!({
                    "depth": depth,
                    "file": file,
                    "line": line,
                    "function": func,
                    "frame_id": frame_id,
                    "locals": locals
                }));
            }
            Ok(serde_json::to_string_pretty(&call_tree)?)
        }

        "step_until_change" => {
            let s = session.ok_or_else(|| anyhow::anyhow!("Session '{session_id}' not found"))?;
            let var_name = args.get("variable_name").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("`variable_name` is required"))?;
            let max_steps = args.get("max_steps").and_then(Value::as_u64).unwrap_or(20) as usize;
            let thread_id = args.get("thread_id").and_then(Value::as_u64).unwrap_or(1) as u32;

            // Get the current value via evaluate
            let get_value = |s: &Arc<crate::dap::session::Session>, tid: u32, expr: &str| {
                let s = s.clone();
                let expr = expr.to_string();
                async move {
                    let st = s.client.request("stackTrace",
                        Some(json!({ "threadId": tid, "startFrame": 0, "levels": 1 }))).await;
                    let frame_id = st.ok()
                        .and_then(|r| r.get("body")?.get("stackFrames")?.as_array()?.first().cloned())
                        .and_then(|f| f.get("id")?.as_u64())
                        .unwrap_or(1) as u32;
                    s.client.request("evaluate", Some(json!({
                        "expression": expr,
                        "frameId": frame_id,
                        "context": "watch"
                    }))).await
                        .ok()
                        .and_then(|r| r.get("body")?.get("result")?.as_str().map(str::to_string))
                        .unwrap_or_else(|| "<error>".to_string())
                }
            };

            let initial_value = get_value(&s, thread_id, var_name).await;
            broadcast_command(hub, session_id, &format!("step_until_change:{var_name}")).await;

            let mut steps = 0usize;
            let mut final_value = initial_value.clone();
            let mut changed = false;
            while steps < max_steps {
                let mut stopped_rx = s.stopped_tx.subscribe();
                s.client.request("next", Some(json!({ "threadId": thread_id }))).await?;
                await_stopped(&s, &mut stopped_rx).await;
                steps += 1;

                final_value = get_value(&s, thread_id, var_name).await;
                if final_value != initial_value {
                    changed = true;
                    break;
                }
            }

            // Get current location for context
            let loc = s.last_stopped.read().await;
            let paused_at = loc.as_ref()
                .and_then(|ev| ev.get("body"))
                .map(|_| {
                    // Read from stack in a best-effort way
                    json!(null)
                });
            drop(loc);

            // Get actual location via stack trace (best-effort)
            let paused_info = s.client.request("stackTrace",
                Some(json!({ "threadId": thread_id, "startFrame": 0, "levels": 1 }))).await
                .ok()
                .and_then(|r| r.get("body")?.get("stackFrames")?.as_array()?.first().cloned())
                .map(|f| json!({
                    "file": f.get("source").and_then(|s| s.get("path")).and_then(Value::as_str).unwrap_or("?"),
                    "line": f.get("line").and_then(Value::as_u64).unwrap_or(0),
                    "function": f.get("name").and_then(Value::as_str).unwrap_or("?")
                }))
                .unwrap_or(paused_at.unwrap_or(json!(null)));

            Ok(serde_json::to_string_pretty(&json!({
                "changed": changed,
                "variable": var_name,
                "from": initial_value,
                "to": final_value,
                "steps_taken": steps,
                "paused_at": paused_info
            }))?)
        }

        _ => Err(anyhow::anyhow!("Unknown tool: {name}")),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_breakpoint_strings(raw: &[String]) -> Vec<(String, Vec<u32>)> {
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

/// Kill a process by PID (best-effort).
fn nix_kill(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On Windows, use taskkill
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn require_u32(args: &Value, key: &str) -> Result<u32> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .ok_or_else(|| anyhow::anyhow!("`{key}` is required and must be an integer"))
}

/// Wait for the next stopped event (up to 15 s) and return a human-readable status string.
/// Subscribe BEFORE sending the DAP command to guarantee the event is never missed.
async fn await_stopped(
    session: &Arc<crate::dap::session::Session>,
    rx: &mut tokio::sync::watch::Receiver<u32>,
) -> String {
    match tokio::time::timeout(std::time::Duration::from_secs(15), rx.changed()).await {
        Err(_) => return "timed out waiting for stop".to_string(),
        Ok(_) => {}
    }
    let guard = session.last_stopped.read().await;
    guard.as_ref()
        .and_then(|ev| ev.get("body"))
        .map(|b| {
            let reason = b.get("reason").and_then(Value::as_str).unwrap_or("step");
            let thread = b.get("threadId").and_then(Value::as_u64).unwrap_or(1);
            format!("paused (reason={reason}, thread={thread}). Call get_debug_context for location.")
        })
        .unwrap_or_else(|| "paused. Call get_debug_context for location.".to_string())
}

/// Returns the threadId of the most recently stopped thread, falling back to 1.
async fn paused_thread_id(session: &Arc<crate::dap::session::Session>) -> u32 {
    let guard = session.last_stopped.read().await;
    guard
        .as_ref()
        .and_then(|ev: &Value| ev.get("body"))
        .and_then(|b: &Value| b.get("threadId"))
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32
}
