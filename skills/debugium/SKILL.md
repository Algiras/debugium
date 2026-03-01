---
name: debugium
description: Use when debugging Python, JavaScript, TypeScript, or Rust code — setting breakpoints, inspecting variables, stepping through execution, evaluating expressions, or tracing bugs with an AI-driven debug session. Triggers on requests like "debug this", "why is this crashing", "set a breakpoint at line X", "inspect the variable", "step through the loop", or "find the bug in my code". Also triggers when the user wants to launch a Debugium debug session, connect MCP tools, or use DAP adapters.
---

# Debugium Debugger Skill

Debugium is a DAP (Debug Adapter Protocol) client with an MCP interface. You can control any active debug session — set breakpoints, step through code, inspect variables, and evaluate expressions — using the tools below.

## Quick Start

### 1. Install Debugium

```bash
curl -fsSL https://raw.githubusercontent.com/Algiras/debugium/main/install.sh | bash
```

### 2. Add to Claude Code MCP config (`~/.claude.json` or `.mcp.json`)

```json
{
  "mcpServers": {
    "debugium": {
      "command": "debugium",
      "args": ["launch", "/abs/path/to/script.py", "--adapter", "python", "--mcp"]
    }
  }
}
```

### 3. Start a debug session

```bash
# Python
debugium launch /abs/path/to/script.py --adapter python --serve --mcp --breakpoint /abs/path/to/script.py:42

# Node.js / JavaScript
debugium launch /abs/path/to/app.js --adapter node --serve --mcp --breakpoint /abs/path/to/app.js:15

# TypeScript (via ts-node)
debugium launch /abs/path/to/app.ts --adapter node --serve --mcp

# Rust (build first with `cargo build`)
debugium launch ./target/debug/my_program --adapter lldb --serve --mcp --breakpoint /abs/path/src/main.rs:60
```

---

## Debugging Workflow

### Standard breakpoint inspection loop

```
1. get_sessions            – verify the session is active
2. set_breakpoints         – place breakpoints at the right lines
3. continue_execution      – run until the next breakpoint
4. get_threads             – find the paused thread_id
5. get_stack_trace         – understand the call stack, get frame_id
6. get_scopes              – get variablesReference for "Locals" scope
7. get_variables           – inspect local variables
8. evaluate                – evaluate any expression in the frame
9. step_over / step_in     – advance one line at a time
10. Repeat from 5 as needed
```

---

## MCP Tools Reference

### `get_sessions`
List all active debug sessions.
```json
{}
```

### `get_source`
Read a source file, optionally zoomed to a line range.
```json
{ "path": "/abs/path/to/file.py", "around_line": 42, "context_lines": 10 }
```

### `set_breakpoints`
Set breakpoints in a file (replaces all existing breakpoints in that file).
```json
{ "session_id": "default", "file": "/abs/path/to/script.py", "lines": [42, 67] }
```

### `continue_execution`
```json
{ "session_id": "default", "thread_id": 1 }
```

### `step_over` / `step_in` / `step_out`
```json
{ "session_id": "default", "thread_id": 1 }
```

### `get_threads`
```json
{ "session_id": "default" }
```

### `get_stack_trace`
```json
{ "session_id": "default", "thread_id": 1, "depth": 20 }
```

### `get_scopes`
```json
{ "session_id": "default", "frame_id": 1 }
```

### `get_variables`
```json
{ "session_id": "default", "variables_reference": 1 }
```

### `evaluate`
```json
{ "session_id": "default", "expression": "len(my_list)", "frame_id": 1, "context": "repl" }
```

### `disconnect`
```json
{ "session_id": "default", "terminate_debuggee": true }
```

---

## Adapter Notes

| Language   | Adapter flag        | Prerequisite                        |
|------------|---------------------|-------------------------------------|
| Python     | `--adapter python`  | `pip install debugpy`               |
| JavaScript | `--adapter node`    | Bundled js-debug                    |
| TypeScript | `--adapter node`    | `ts-node` in PATH                   |
| Rust       | `--adapter lldb`    | `codelldb` in PATH + `cargo build`  |

---

## Tips for Effective AI Debugging

1. **Always `get_source` first** — know exact line numbers before setting breakpoints.
2. **Use `evaluate` liberally** — cheaper than stepping; narrow down bugs fast.
3. **Check the whole stack** — `get_stack_trace` reveals library frames to step out of.
4. **Drill into nested variables** — any `variablesReference > 0` has children; call `get_variables` recursively.
5. **Set multiple breakpoints upfront** — one `set_breakpoints` call with all lines is more efficient.
