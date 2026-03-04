---
name: Debugium DAP Debugger
description: Drive live debug sessions for Python, Node.js, TypeScript, C, C++, Rust, Java, Scala, and WebAssembly using the Debugium MCP tools. Use when asked to debug code, set breakpoints, inspect variables, step through execution, trace bugs, or find why something crashes.
---

# Debugium Debugger Skill

Debugium is a DAP (Debug Adapter Protocol) client with an MCP interface and a real-time web UI. You can control any active debug session — set breakpoints, step through code, inspect live values, record findings, and annotate the source editor.

---

## Setup & Connection

### 1. Install

```bash
cargo install --path crates/debugium-server
```

### 2. Register MCP server

Add to `.mcp.json` (project root) or `~/.claude.json`:
```json
{
  "mcpServers": {
    "debugium": { "command": "debugium", "args": ["mcp"] }
  }
}
```

### 3. Install language prerequisites

| Language | Install |
|----------|---------|
| Python | `pip install debugpy` |
| Node.js / TypeScript | js-debug (build from microsoft/vscode-js-debug) + `npm i -g tsx` for TS |
| C / C++ / Rust | `lldb-dap` (ships with LLVM: `brew install llvm`) |
| Java | microsoft/java-debug adapter JAR |
| Scala | Running Metals language server with DAP |

### 4. Launch a debug session

**Preferred**: use a `dap.json` config from `examples/`. All paths must be absolute.

```bash
# Python — multiple breakpoints with -b
debugium launch /abs/path/script.py --config examples/python.dap.json \
  -b /abs/path/script.py:42 -b /abs/path/script.py:67

# Comma-separated lines in one file
debugium launch /abs/path/script.py --adapter python \
  --breakpoint /abs/path/script.py:10,15,20

# Node.js
debugium launch /abs/path/app.js --config examples/node.dap.json \
  -b /abs/path/app.js:15 -b /abs/path/app.js:30

# TypeScript
debugium launch /abs/path/app.ts --config examples/typescript.dap.json \
  -b /abs/path/app.ts:10

# C / C++ (compile with debug symbols first: cc -g -O0)
debugium launch /tmp/a.out --config examples/c-cpp.dap.json \
  -b /abs/path/main.c:20

# Rust (cargo build first)
debugium launch ./target/debug/myapp --config examples/c-cpp.dap.json \
  -b /abs/path/src/main.rs:10

# Remote attach (debugpy already listening on 127.0.0.1:5678)
python3 -m debugpy --listen 127.0.0.1:5678 --wait-for-client app.py &
debugium launch app.py --config examples/remote-python.dap.json \
  -b /abs/path/app.py:42
```

**Shorthand** (Python only): `debugium launch script.py --adapter python -b ...`

**Auto-discovery**: place `dap.json` in project root, then just `debugium launch program -b ...`

### 5. Verify connection

Call `get_sessions` — if empty, the server isn't running. Launch a session first.

---

## Web UI

The debugger launches a web UI automatically (unless `--no-open-browser`). Features:

- **Layout presets**: Slim (source only), Std (console collapsed), Full (everything open)
- **Light/Dark mode**: toggle with Ctrl/Cmd+D or the header button
- **Panels**: Source, Console, Variables, Stack, Breakpoints, Watch, Findings, Timeline
- **Icon toolbar**: Continue, Pause, Step In/Over/Out, Stop, Restart (F-key shortcuts)
- **AI activity**: LLM tool calls are shown in real-time via the console

---

## Debugging Workflow

### Standard loop

```
1. launch_session        – start a debug session (or attach_session for remote targets)
   attach_session        – attach to a running process (debugpy, JDWP, Node inspector)
2. get_debug_context     – orient: paused_at + locals + call_stack + source_window + breakpoints in ONE call
3. evaluate / get_variables  – inspect specific values
4. step_over / step_in   – advance (blocking: waits for pause, safe to chain)
5. get_debug_context     – re-orient after stepping
6. annotate / add_finding – record conclusions in the UI
7. Repeat 3–6 as needed
8. stop_session          – clean up when done
```

**Key insight**: `get_debug_context` replaces the old 7-call chain of
`get_threads → get_stack_trace → get_scopes → get_variables`. Use it first.

---

## Tool Reference

### Session

#### `launch_session`
Launch a new debug session autonomously — no human intervention needed. Spawns the adapter, sets breakpoints, waits until paused.
```json
{ "program": "/abs/path/script.py", "adapter": "python", "breakpoints": ["/abs/path/script.py:42"] }
```
Returns `{ "session_id": "...", "status": "paused" | "running" }`. Use the returned `session_id` for all subsequent tool calls.

#### `attach_session`
Attach to a running debug target (JVM via JDWP, Python via debugpy, Node via inspector, or a remote DAP server). Spawns or connects a debug adapter, sets breakpoints, waits until paused.
```json
{ "port": 5005, "adapter": "java", "host": "127.0.0.1", "breakpoints": ["/abs/path/App.java:42"] }
```
Parameters:
- `port` (required): Remote debug port (e.g. 5005 for JDWP, 5678 for debugpy, 9229 for Node inspector)
- `adapter`: `"java"`, `"python"`, `"node"`, `"lldb"` — auto-detected from program extension if omitted
- `host`: Remote host (default: `"127.0.0.1"`)
- `program`: Path to source file (for breakpoints and source context)
- `breakpoints`: Array of `"file:line"` strings
- `attach_args`: Custom args merged into the DAP attach request (overrides defaults)
- `session_id`: Optional custom session ID

Returns `{ "session_id": "...", "status": "paused" | "running" }`.

#### `stop_session`
Stop and clean up a debug session. Sends disconnect, kills adapter, removes from registry.
```json
{ "session_id": "session-123" }
```

#### `get_sessions` / `list_sessions`
List active sessions. Empty = server not started.
```json
{}
```

---

### Source

#### `get_source`
Read a source file with optional line zoom.
```json
{ "path": "/abs/path/to/file.py", "around_line": 42, "context_lines": 10 }
```

---

### Breakpoints

#### `set_breakpoints`
Set breakpoints in a file (replaces all existing in that file).
```json
{ "file": "/abs/path/to/script.py", "lines": [42, 67, 103] }
```

#### `set_breakpoint`
Set or update a single breakpoint with an optional condition.
```json
{ "file": "/abs/path/to/script.py", "line": 42, "condition": "x > 10" }
```

#### `list_breakpoints` / `clear_breakpoints`
```json
{}
```

#### `set_function_breakpoints`
Break on a function name.
```json
{ "names": ["my_function", "ClassName.method"] }
```

#### `set_exception_breakpoints`
```json
{ "filter": "raised" }   // or "uncaught"
```

---

### Execution Control

#### `continue_execution`
Resume until the next breakpoint. Returns `console_line_count` — pass it to
`wait_for_output` as `from_line` to avoid matching stale output.
```json
{ "thread_id": 1 }
```
Returns: `{ "status": "running", "console_line_count": 42, "hint": "..." }`

#### `step_over` / `step_in` / `step_out`
**Blocking** — waits for the adapter to pause before returning. Safe to chain
back-to-back without sleeps. Returns confirmation + "Call get_debug_context for location."
```json
{ "thread_id": 1 }
```

#### `pause` / `restart` / `terminate` / `disconnect`
```json
{ "thread_id": 1 }
```

---

### Inspection (use when paused)

#### `get_debug_context` ★ START HERE
Single call returning: `paused_at`, `locals`, `call_stack`, `source_window (±5 lines)`,
`breakpoints`, `frame_id`, `thread_id`. Replaces the old get_threads→stack→scopes→vars chain.
```json
{}
```

#### `get_threads`
```json
{}
```

#### `get_stack_trace`
```json
{ "thread_id": 1, "depth": 20 }
```
Returns frames with `id` (frame_id), `name`, `line`, `source`.

#### `get_scopes`
```json
{ "frame_id": 2 }
```
Returns scopes (Locals, Globals) each with `variablesReference`.

#### `get_variables`
```json
{ "variables_reference": 6 }
```
Nested objects have their own `variablesReference` — call recursively to drill in.

#### `evaluate`
Evaluate any expression in the current frame.
```json
{ "expression": "len(my_list)", "frame_id": 2, "context": "repl" }
```

#### `set_variable`
Mutate a variable in the current scope.
```json
{ "variables_reference": 6, "name": "counter", "value": "0" }
```

#### `get_exception_info`
Details about the current exception (when stopped on exception).
```json
{}
```

#### `get_capabilities`
What the adapter supports.
```json
{}
```

---

### Console Output

#### `get_console_output`
Last N lines of stdout/stderr.
```json
{ "lines": 50 }
```

#### `wait_for_output`
Poll until stdout matches a regex (or timeout). Use `from_line` from
`continue_execution` to only match **new** output — not output from earlier in the session.
```json
{ "pattern": "Error.*line", "from_line": 42, "timeout_secs": 10 }
```

---

### History & Timeline

#### `get_timeline`
Every stop in this session: file, line, changed variables, stack summary.
```json
{ "limit": 50 }
```

#### `get_variable_history`
How one variable's value changed across all stops. Answers "when did X go wrong?"
```json
{ "name": "counter" }
```

---

### Annotations & Findings (record conclusions in the UI)

#### `annotate`
Pin a colored marker on a source line, visible to the human in the editor.
```json
{ "file": "/abs/path/to/file.py", "line": 42, "message": "off-by-one here", "color": "red" }
```

#### `get_annotations`
Read back all annotations you've already placed — do this at session start to avoid
re-investigating known lines.
```json
{}
```

#### `add_finding`
Record a structured conclusion in the Findings panel.
```json
{ "message": "counter overflows at iteration 256", "level": "error" }
```
Levels: `info`, `warning`, `error`.

#### `get_findings`
Read back all findings — do this at session start to avoid restating known conclusions.
```json
{}
```

---

### Watch Expressions

#### `add_watch` / `remove_watch`
Expressions evaluated automatically at every stop.
```json
{ "expression": "len(queue)" }
```

#### `get_watches`
Current expressions + their last evaluated values.
```json
{}
```

---

### Compound / LLM Tools

#### `step_until`
Step until an expression becomes true (up to max_steps).
```json
{ "condition": "i == 10", "max_steps": 50 }
```

#### `run_until_exception`
Continue until any exception is raised.
```json
{}
```

#### `compare_snapshots`
Diff variable snapshots between two timeline stops.
```json
{ "stop_a": 3, "stop_b": 7 }
```

#### `find_first_change`
Find the first timeline stop where a variable changed (optionally from an expected value).
```json
{ "variable_name": "counter", "expected_value": "0" }
```

#### `get_call_tree`
Stack + locals for each frame in one call.
```json
{ "max_depth": 5 }
```

#### `step_until_change`
Step until a variable's value changes.
```json
{ "variable_name": "status", "max_steps": 20 }
```

#### `explain_exception`
When stopped on an exception, gather all relevant context in one call.
```json
{}
```

#### `restart_frame`
Re-run execution from a specific stack frame (requires `supportsRestartFrame`).
```json
{ "frame_id": 2 }
```

---

### Navigation & Source Discovery

#### `goto_targets`
Get valid jump targets for a given source location (requires `supportsGotoTargetsRequest`).
```json
{ "file": "/abs/path/to/file.py", "line": 43 }
```

#### `goto`
Jump execution to a target without running intermediate code.
```json
{ "thread_id": 1, "target_id": 0 }
```

#### `breakpoint_locations`
Get valid breakpoint positions in a line range (requires `supportsBreakpointLocationsRequest`).
```json
{ "file": "/abs/path/to/file.js", "line": 30, "end_line": 40 }
```

#### `step_in_targets`
List possible step-in targets when a line has multiple calls (requires `supportsStepInTargetsRequest`).
```json
{ "frame_id": 0 }
```

#### `loaded_sources`
List all source files currently loaded by the adapter (requires `supportsLoadedSourcesRequest`).
```json
{}
```

#### `source_by_reference`
Fetch source code for generated/internal code by sourceReference ID.
```json
{ "source_reference": 2017626721 }
```

---

### Mutation

#### `set_expression`
Set the value of an expression (requires `supportsSetExpression`).
```json
{ "expression": "obj.field", "value": "42", "frame_id": 0 }
```

---

### Memory & Disassembly (native debugging)

#### `read_memory`
Read raw bytes from debuggee memory (requires `supportsReadMemoryRequest`).
```json
{ "memory_reference": "0x7fff5000", "count": 128 }
```

#### `write_memory`
Write raw bytes to debuggee memory (requires `supportsWriteMemoryRequest`).
```json
{ "memory_reference": "0x7fff5000", "data": "AQIDBA==" }
```

#### `disassemble`
Disassemble machine instructions (requires `supportsDisassembleRequest`).
```json
{ "memory_reference": "0x100003f00", "instruction_count": 20 }
```

---

### Control

#### `cancel_request`
Cancel an in-flight request (requires `supportsCancelRequest`).
```json
{ "request_id": 42 }
```

---

### Data Breakpoints (watchpoints)

#### `set_data_breakpoint`
Break when a variable is written/read.
```json
{ "name": "counter", "access_type": "write" }
```

#### `list_data_breakpoints` / `clear_data_breakpoints`
```json
{}
```

---

### Session Persistence

#### `export_session`
Export breakpoints, annotations, findings, and watches as a JSON bundle.
```json
{}
```

#### `import_session`
Restore exported state into the current session.
```json
{ "data": { "..." } }
```

---

## Supported Adapters

| Language     | `--adapter` flag              | Prerequisite                          | Verified |
|-------------|-------------------------------|---------------------------------------|----------|
| Python      | `python` / `debugpy`          | `pip install debugpy`                 | ✅ |
| Node.js     | `node` / `js`                 | js-debug (bundled)                    | ✅ |
| TypeScript  | `typescript` / `ts` / `tsx`   | js-debug + `tsx` or `ts-node`         | ✅ |
| C / C++     | `lldb` / `codelldb`           | `lldb-dap`                            | ✅ |
| Rust        | `lldb` / `rust`               | `lldb-dap` + `cargo build`            | ✅ |
| Java        | `java` / `jvm`                | microsoft/java-debug adapter JAR      | ✅ |
| Scala       | `--config scala-jvm.dap.json` | `scalac` + Scala library JAR          | ✅ |
| WebAssembly | `--config wasm.dap.json`      | `wasmtime` + `lldb-dap` (LLVM ≥16)   | ✅ |
| Any adapter | `--config dap.json`           | See `dap.json.example`                | ✅ |

Remote attach is supported via `attach_session` MCP tool or `dap.json` with `host` + `port` fields.

---

## CLI Breakpoint Syntax

```bash
# Repeated -b flags (short for --breakpoint)
debugium launch app.py --adapter python -b app.py:10 -b app.py:20 -b other.py:5

# Comma-separated lines in one file
debugium launch app.py --adapter python --breakpoint app.py:10,15,20

# Mix both
debugium launch app.py --adapter python -b app.py:10,20 -b other.py:5
```

---

## Tips

1. **Start with `get_debug_context`** — not `get_source`. Live runtime values beat static reading.
2. **Steps are blocking** — chain `step_over` calls freely; each confirms the pause before returning.
3. **Thread `console_line_count`** — pass it from `continue_execution` into `wait_for_output` as `from_line` to avoid false positives on old output.
4. **Read before re-investigating** — call `get_annotations` and `get_findings` at the start of each session to see what you already know.
5. **Use `evaluate` before stepping** — cheaper than advancing line-by-line. Narrow the bug first.
6. **Use `step_until`** — instead of manually looping, let the tool advance until your condition fires.
7. **Drill nested variables** — any `variablesReference > 0` has children; call `get_variables` recursively.
8. **Set all breakpoints at once** — one `set_breakpoints` call beats many individual ones.
