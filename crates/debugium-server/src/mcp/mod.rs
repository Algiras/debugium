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

mod tools;
pub use tools::dispatch_tool;
pub use tools::broadcast_breakpoints_changed;

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
    ("restart_frame",            "supportsRestartFrame"),
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
                        "thread_id": { "type": "integer", "description": "Thread to step. Auto-detected from last stopped event if omitted." }
                    },
                    "required": []
                }
            },
            {
                "name": "step_in",
                "description": "Step into the function call on the current line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to step. Auto-detected from last stopped event if omitted." }
                    },
                    "required": []
                }
            },
            {
                "name": "step_out",
                "description": "Step out of the current function, returning to the caller.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to step. Auto-detected from last stopped event if omitted." }
                    },
                    "required": []
                }
            },
            {
                "name": "pause",
                "description": "Pause execution of a running thread.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to pause. Auto-detected from last stopped event if omitted." }
                    },
                    "required": []
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
                        "thread_id": { "type": "integer", "description": "Auto-detected from last stopped event if omitted." },
                        "depth": { "type": "integer", "description": "Max frames to return. Default 20." }
                    },
                    "required": []
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
                "description": "Get a compact, LLM-optimized snapshot of the current debug state in one call: paused location, local variables (auto-expanded 1 level for objects), call stack summary, source window (±5 lines), active breakpoints, watch results, and what changed since the last stop. Use this instead of separate get_stack_trace + get_scopes + get_variables calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to inspect. Default 1." },
                        "verbosity": { "type": "string", "enum": ["compact", "full"], "description": "compact = top 10 vars + 3 frames; full = top 30 vars + all frames. Default: full." },
                        "expand_depth": { "type": "integer", "description": "Auto-expand nested variables up to this depth (0 = flat, 1 = one level). Default: 1." }
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
                "description": "Step over repeatedly until a runtime expression evaluates to truthy in the debuggee's language scope (e.g. Python: x > 5, JavaScript: items.length > 0), or until max_steps is reached. The condition is evaluated in the current stack frame — not debugger metadata. Much more efficient than calling step_over + evaluate in a loop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "condition": { "type": "string", "description": "Runtime expression in the debuggee's language, evaluated in the current stack frame after each step (e.g. Python: 'x > 5', JavaScript: 'i < arr.length'). Uses variables and syntax of the target language." },
                        "max_steps": { "type": "integer", "description": "Maximum steps before giving up. Default 20." },
                        "thread_id": { "type": "integer", "description": "Thread to step. Default 1." }
                    },
                    "required": ["condition"]
                }
            },
            {
                "name": "continue_until",
                "description": "Run to a specific line (like 'run to cursor'). Sets a temporary breakpoint at the target line, continues execution, waits for the stop, then removes the temporary breakpoint and returns the debug context. Much faster than manually managing breakpoints.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number to run to (1-indexed)." },
                        "thread_id": { "type": "integer", "description": "Thread to continue. Default 1." },
                        "timeout_secs": { "type": "integer", "description": "Max seconds to wait for the stop. Default 15." }
                    },
                    "required": ["file", "line"]
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
                "name": "explain_exception",
                "description": "When stopped on an exception, gather all relevant context in one call: exception info, paused location, locals, call stack, recent console output, source window, and recent timeline. Returns a structured diagnosis. Use this instead of calling get_exception_info + get_debug_context + get_console_output separately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "thread_id": { "type": "integer", "description": "Thread to inspect. Auto-detected if omitted." }
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
                "name": "set_data_breakpoint",
                "description": "Set a data breakpoint (watchpoint) — break when a variable's value changes, is read, or is written. First queries the adapter for eligibility via dataBreakpointInfo, then sets the breakpoint. Much faster than step_until_change for finding when a variable gets corrupted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "name": { "type": "string", "description": "Variable name to watch." },
                        "variables_reference": { "type": "integer", "description": "The variablesReference of the scope/object containing the variable. Use 0 or omit to search by name only." },
                        "access_type": { "type": "string", "enum": ["write", "read", "readWrite"], "description": "When to break. Default: 'write'." },
                        "condition": { "type": "string", "description": "Optional condition expression." },
                        "hit_condition": { "type": "string", "description": "Optional hit count condition." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "list_data_breakpoints",
                "description": "List all active data breakpoints (variable watchpoints).",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "clear_data_breakpoints",
                "description": "Remove all data breakpoints.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "get_capabilities",
                "description": "Get the adapter capabilities returned during initialize. Useful to check if features like function breakpoints, exception info, data breakpoints, or completions are supported.",
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
                        "thread_id": { "type": "integer", "description": "Auto-detected from last stopped event if omitted." }
                    },
                    "required": []
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
                "description": "Set a single breakpoint at a specific file and line. Optionally specify a condition, hit condition, or log message. Returns the verified line number from the adapter.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number to break on (1-indexed)." },
                        "condition": { "type": "string", "description": "Optional condition expression — only pause when this evaluates to true." },
                        "hit_condition": { "type": "string", "description": "Optional hit count condition — e.g. '== 5' or '>= 10'. Breaks when the hit count satisfies this expression." },
                        "log_message": { "type": "string", "description": "Optional log message template — turns this into a logpoint that logs instead of stopping. Use {expr} for interpolation, e.g. 'x={x}, len={len(items)}'." }
                    },
                    "required": ["file", "line"]
                }
            },
            {
                "name": "set_logpoint",
                "description": "Set a logpoint — a non-stopping breakpoint that logs a message template to the console. The program does NOT pause; the message is printed to debug console output. Use {expression} syntax for interpolation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "file": { "type": "string", "description": "Absolute path to the source file." },
                        "line": { "type": "integer", "description": "Line number for the logpoint (1-indexed)." },
                        "message": { "type": "string", "description": "Log message template. Use {expression} for interpolation, e.g. 'counter={counter}, items={len(items)}'." },
                        "condition": { "type": "string", "description": "Optional condition — only log when this evaluates to true." },
                        "hit_condition": { "type": "string", "description": "Optional hit count condition — e.g. '>= 10'. Only log when the hit count satisfies this." }
                    },
                    "required": ["file", "line", "message"]
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
                "name": "export_session",
                "description": "Export the current session's accumulated debugging knowledge: breakpoints, annotations, findings, and watch expressions. Returns a JSON bundle that can be imported into a new session on the same codebase.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": []
                }
            },
            {
                "name": "import_session",
                "description": "Import previously exported debugging knowledge into the current session: restores breakpoints, annotations, findings, and watches. Use after re-launching a session on the same codebase.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "data": { "type": "object", "description": "The JSON bundle from export_session." }
                    },
                    "required": ["data"]
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
                "description": "Read raw memory from the debuggee process. Primarily for native debugging (C/C++/Rust). Returns base64-encoded bytes. Requires adapter support (supportsReadMemoryRequest).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "memory_reference": { "type": "string", "description": "Memory reference (hex address or expression)." },
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
            },
            {
                "name": "restart_frame",
                "description": "Restart execution from a specific stack frame. Re-runs the function from its beginning without restarting the whole session. Requires adapter support (supportsRestartFrame).",
                "inputSchema": {
                    "type": "object",
                    "required": ["frame_id"],
                    "properties": {
                        "frame_id":   { "type": "integer", "description": "Stack frame ID to restart (from get_stack_trace)." },
                        "session_id": { "type": "string" }
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
    mcp_ctx: &McpContext,
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

        // Tool discovery
        "tools/list" => {
            let adapter_caps = if let Some(port) = proxy_port {
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
            let result = dispatch_tool(&name, args, registry, hub).await;
            // Session-changing tools alter the available tool set (capability gating)
            if matches!(name.as_str(), "launch_session" | "stop_session") && result.is_ok() {
                let notif = serde_json::json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"});
                let mut msg = notif.to_string();
                msg.push('\n');
                let _ = mcp_ctx.outbox.send(msg).await;
            }
            match result {
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
