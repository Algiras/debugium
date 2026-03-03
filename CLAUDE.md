# Debugium — Claude Code Instructions

Debugium is a live debugger with an MCP bridge. When the `debugium` MCP server is
available, prefer it over reading source files manually — you get live runtime values,
not static guesses.

---

## Setup

### 1. Install Debugium

```bash
cargo install --path crates/debugium-server
```

### 2. Register the MCP server

Add to `.mcp.json` (project root) or `~/.claude.json`:
```json
{
  "mcpServers": {
    "debugium": { "command": "debugium", "args": ["mcp"] }
  }
}
```

### 3. Launch a debug session

Preferred: use a `dap.json` config from `examples/`:

```bash
# Python — multiple breakpoints with -b (short for --breakpoint)
debugium launch script.py --config examples/python.dap.json \
  -b "$(pwd)/script.py:42" -b "$(pwd)/script.py:67"

# Comma-separated lines in one file
debugium launch script.py --adapter python --breakpoint "$(pwd)/script.py:10,15,20"

# Node.js
debugium launch app.js --config examples/node.dap.json \
  -b "$(pwd)/app.js:15" -b "$(pwd)/app.js:30"

# TypeScript
debugium launch app.ts --config examples/typescript.dap.json -b "$(pwd)/app.ts:10"

# C / C++ (compile with -g -O0 first)
cc -g -O0 main.c -o /tmp/main
debugium launch /tmp/main --config examples/c-cpp.dap.json -b "$(pwd)/main.c:20"

# Rust
cargo build && debugium launch ./target/debug/myapp --config examples/rust.dap.json \
  -b "$(pwd)/src/main.rs:10"

# Remote attach (debugpy already listening on port 5678)
debugium launch app.py --config examples/remote-python.dap.json -b "$(pwd)/app.py:42"
```

Or use `--adapter` shorthand (Python only is built-in):
```bash
debugium launch script.py --adapter python -b "$(pwd)/script.py:42"
```

Auto-discovery: place a `dap.json` in the project root and omit `--config`/`--adapter`.

### 4. Or launch a session via MCP (autonomous)

Instead of CLI launch, use the `launch_session` MCP tool directly:
```
launch_session(program="/abs/path/script.py", breakpoints=["/abs/path/script.py:42"])
→ { session_id: "session-...", status: "paused" }
```

### 5. Verify the server is running

```
get_sessions          → lists active sessions (empty = server not started)
```

If empty and `launch_session` is not available, ask the user to launch manually.

---

## Standard debugging workflow

### 0. Launch (if no session exists)
```
launch_session        → starts adapter, sets breakpoints, returns session_id
```

### 1. Orient — always start here
```
get_debug_context     → paused_at, locals, call_stack, source_window, breakpoints
```
Never assume location from source alone. Always call this first when paused.

### 2. Inspect deeper if needed
```
get_stack_trace       → full call stack with frame IDs
get_scopes            → scope list for a frame
get_variables         → expand a variablesReference
evaluate              → eval arbitrary expression in current frame
```

### 3. Step — blocking, safe to chain
`step_over`, `step_in`, `step_out` now **block until paused** and confirm the stop.
`thread_id` is auto-detected from the last stopped event — no need to pass it.
Chain them freely — each call waits for the previous step to complete:
```
step_over → "paused (reason=step, thread=1). Call get_debug_context for location."
step_over → same
get_debug_context → now shows the new location
```

### 4. Continue + wait for output
`continue_execution` returns `console_line_count`. Pass it to `wait_for_output`
as `from_line` to only match output printed **after** the resume — not stale history:
```
continue_execution                              → { console_line_count: 42, ... }
wait_for_output("Error", from_line=42, timeout_secs=10)  → matched/timed out
```
Without `from_line`, the whole buffer is searched (fine for fresh sessions).

### 5. Track history across stops
```
get_timeline              → every stop: file, line, changed vars, stack summary
get_variable_history(name) → values a variable held across all stops ("when did x break?")
```

### 6. Annotate conclusions in the UI
When you find something interesting, make it visible to the human:
```
annotate(file, line, message, color)   → colored gutter marker in the editor
add_finding(message, level)            → appears in the Findings panel
```
Then on your **next invocation**, read back what you already concluded:
```
get_annotations   → "2 annotations" — avoids re-investigating known lines
get_findings      → "1 finding" — avoids re-stating known conclusions
```

### 7. Watch expressions
```
add_watch(expression)   → evaluated at every stop, shown in Watch panel
get_watches             → current expressions + last values
remove_watch(expression)
```

---

## Decision tree for common questions

| Question | Tool |
|----------|------|
| Where am I? | `get_debug_context` |
| What changed since last stop? | `get_timeline` (changed_vars field) |
| What changed between stop 3 and stop 7? | `compare_snapshots(3, 7)` |
| When did variable X first change? | `find_first_change("x")` |
| What are locals in every frame? | `get_call_tree(max_depth=5)` |
| Step until variable X changes? | `step_until_change("x")` |
| When did variable X go wrong? | `get_variable_history("x")` |
| Did the program print Y? | `wait_for_output("Y", from_line=N)` |
| What did I already annotate? | `get_annotations` |
| What did I already conclude? | `get_findings` |
| Multiple sessions? | `list_sessions`, then pass `session_id` to all tools |

---

## Anti-patterns to avoid

- **Don't read source files to guess values** — use `evaluate` or `get_variables` for live data
- **Don't call `step_over` in a loop without `get_debug_context`** — step, then orient
- **Don't call `wait_for_output` without `from_line`** after a long session — you'll match old output
- **Don't re-annotate lines you already marked** — call `get_annotations` first
- **Don't repeat findings you already recorded** — call `get_findings` at session start

---

## Tool quick-reference

```
# Session
launch_session(program, adapter?, config?, breakpoints?, session_id?)
stop_session(session_id?)
get_sessions / list_sessions
get_capabilities                  (adapter feature flags — what's supported)

# Breakpoints
set_breakpoints(file, lines[])
set_breakpoint(file, line, condition?)
set_logpoint(file, line, log_message)
set_function_breakpoints(names[]) (requires supportsFunctionBreakpoints)
set_exception_breakpoints(filters[])
set_data_breakpoint(variable)     (requires supportsDataBreakpoints)
list_breakpoints / clear_breakpoints
list_data_breakpoints / clear_data_breakpoints
continue_until(file, line)        (temp breakpoint + continue)

# Execution — thread_id auto-detected from last stopped event if omitted
step_over / step_in / step_out
continue_execution                (returns console_line_count)
pause / disconnect / terminate / restart

# Inspection
get_debug_context                 (START HERE — locals + stack + source window)
get_stack_trace / get_scopes / get_variables / get_threads
evaluate(expression, frame_id)
get_source(file)
get_console_output
get_exception_info                (requires supportsExceptionInfoRequest)
set_variable(variablesReference, name, value)

# Output
wait_for_output(pattern, from_line=0, timeout_secs=10)

# Memory & disassembly (native debugging)
read_memory(memory_reference, count)   (requires supportsReadMemoryRequest)
write_memory(memory_reference, data)   (requires supportsWriteMemoryRequest)
disassemble(memory_reference, count)   (requires supportsDisassembleRequest)

# History
get_timeline(limit=50)
get_variable_history(name)

# Annotations & findings
annotate(file, line, message, color)
get_annotations
add_finding(message, level)
get_findings

# Watches
add_watch / remove_watch / get_watches

# Session persistence
export_session → JSON bundle of state
import_session(data) → restore from bundle

# Compound
explain_exception                 (exceptionInfo + stack + locals in one call)
step_until(condition, max_steps)  (condition is a runtime expression, e.g. Python: x > 5)
run_until_exception
compare_snapshots(stop_a, stop_b) (diff variable snapshots between two timeline stops)
find_first_change(variable_name, expected_value?)  (first stop where variable changed)
get_call_tree(max_depth=5)        (stack + locals for each frame in one call)
step_until_change(variable_name, max_steps=20)     (step until variable value changes)
```
