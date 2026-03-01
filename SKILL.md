---
name: Debugium DAP Debugger
description: Drive live debug sessions for Python, Node.js, TypeScript, and Rust using the Debugium MCP tools. Use when asked to debug code, set breakpoints, inspect variables, step through execution, trace bugs, or find why something crashes.
---

# Debugium Debugger Skill

Debugium is a DAP (Debug Adapter Protocol) client with an MCP interface. You can control any active debug session — set breakpoints, step through code, inspect live values, record findings, and annotate the source editor.

---

## Quick Start

### 1. Launch a session

```bash
# Python
debugium launch /abs/path/to/script.py --adapter python --breakpoint /abs/path/to/script.py:42

# Node.js / JavaScript
debugium launch /abs/path/to/app.js --adapter node --breakpoint /abs/path/to/app.js:15

# TypeScript (via ts-node)
debugium launch /abs/path/to/app.ts --adapter node

# Rust (build first)
cargo build && debugium launch ./target/debug/my_program --adapter lldb --breakpoint /abs/path/src/main.rs:60
```

### 2. Add to `.mcp.json` (project root) or `~/.claude.json`

```json
{
  "mcpServers": {
    "debugium": {
      "command": "debugium",
      "args": ["mcp"]
    }
  }
}
```

---

## Debugging Workflow

### Standard loop

```
1. get_sessions          – confirm session is active (empty = server not running)
2. get_debug_context     – orient: paused_at + locals + call_stack + source window + breakpoints in ONE call
3. evaluate / get_variables  – inspect specific values
4. step_over / step_in   – advance (blocking: waits for pause, safe to chain)
5. get_debug_context     – re-orient after stepping
6. annotate / add_finding – record conclusions in the UI
7. Repeat 3–6 as needed
```

**Key insight**: `get_debug_context` replaces the old 7-call chain of
`get_threads → get_stack_trace → get_scopes → get_variables`. Use it first.

---

## Tool Reference

### Session

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

---

## Adapter Notes

| Language   | Flag                | Prerequisite                       |
|------------|---------------------|------------------------------------|
| Python     | `--adapter python`  | `pip install debugpy`              |
| JavaScript | `--adapter node`    | Bundled js-debug                   |
| TypeScript | `--adapter node`    | `ts-node` in PATH                  |
| Rust       | `--adapter lldb`    | `codelldb` in PATH + `cargo build` |

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
