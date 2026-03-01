# Debugium — Claude Code Instructions

Debugium is a live debugger with an MCP bridge. When the `debugium` MCP server is
available, prefer it over reading source files manually — you get live runtime values,
not static guesses.

---

## Prerequisite: server must be running

The MCP tools do nothing unless a Debugium server is active. Check first:

```
get_sessions          → lists active sessions (empty = server not started)
```

If empty, ask the user to run:
```bash
debugium launch <script.py> --adapter python --breakpoint <file>:<line>
```

---

## Standard debugging workflow

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
get_sessions / list_sessions

# Breakpoints
set_breakpoint(file, line, condition?)
set_breakpoints(file, lines[])
list_breakpoints / clear_breakpoints

# Execution — all blocking except continue
step_over / step_in / step_out    (thread_id=1 default)
continue_execution                (returns console_line_count)
pause / disconnect

# Inspection
get_debug_context                 (START HERE)
get_stack_trace / get_scopes / get_variables / get_threads
evaluate(expression, frame_id)
get_console_output

# Output
wait_for_output(pattern, from_line=0, timeout_secs=10)

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

# Compound
get_debug_context                 (locals + stack + source window in one call)
step_until(condition, max_steps)
run_until_exception
```
