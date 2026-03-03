# Changelog

All notable changes to Debugium are documented here.

## [0.2.0] — 2026-03-03

### DAP Protocol Coverage (35 requests implemented)

- **breakpointLocations** — query valid breakpoint positions in a source range
- **stepInTargets** — list possible step-in targets at current location
- **setExpression** — set value via expression (complement to setVariable)
- **loadedSources** — list all loaded source files from the adapter
- **source** (by reference) — fetch generated/internal code by sourceReference
- **restartFrame** — re-run execution from a specific stack frame
- **goto / gotoTargets** — jump execution to a line without running intermediate code
- **cancel** — cancel in-flight DAP requests
- All new tools are **capability-gated** — only exposed when the adapter supports them

### UI Improvements

- **Run to cursor** — right-click gutter popover with "Run to cursor" button
- **Logpoint indicator** — diamond icon + log message shown in Breakpoints panel
- **Per-breakpoint removal** — "✕" button on each breakpoint in the sidebar
- **Thread selector** — dropdown in Stack panel header for multi-threaded programs
- **Recursive variable expansion** — expand nested objects 2+ levels deep in Variables panel
- **Rich breakpoint metadata** — condition and logMessage preserved across WS updates

### Backend

- **Child session routing** — js-debug parent/child sessions fully handled (startDebugging)
- **Capability-gated tools/list** — MCP tools dynamically filtered based on adapter capabilities
- **Auto thread-id detection** — stepping/inspection tools auto-resolve from last stopped event
- Dead code removal, signal handling fix for non-Unix platforms

## [0.1.0] — 2026-02-26

### Core

- Multi-language debugging via DAP: Python, Node.js, TypeScript, C/C++, Rust, Java, Scala, WASM
- Real-time web UI with 9 panels: Source, Stack, Variables, Console, Breakpoints, Findings, Watch, Timeline, Sessions
- MCP integration with 40+ tools for LLM-driven debugging
- CLI control commands for headless operation

### MCP Tools

- **Inspection**: get_debug_context, get_stack_trace, get_scopes, get_variables, evaluate, get_threads, get_source, get_capabilities, get_exception_info
- **Breakpoints**: set_breakpoint, set_breakpoints, set_logpoint, set_function_breakpoints, set_exception_breakpoints, set_data_breakpoint, list/clear breakpoints
- **Execution**: continue_execution, step_over, step_in, step_out, pause, disconnect, terminate, restart
- **Compound**: step_until, step_until_change, continue_until, run_until_exception, explain_exception, get_call_tree, compare_snapshots, find_first_change
- **History**: get_timeline, get_variable_history
- **Annotations**: annotate, get_annotations, add_finding, get_findings
- **Watches**: add_watch, remove_watch, get_watches
- **Output**: get_console_output, wait_for_output
- **Memory**: read_memory, write_memory, disassemble
- **Session**: launch_session, stop_session, get_sessions, list_sessions, export_session, import_session

### UI

- CodeMirror 6 editor with syntax highlighting for Python, JavaScript, TypeScript, Rust
- Breakpoint gutter with click-to-toggle and right-click condition popover
- Inline variable values on execution line
- Execution arrow and line highlighting
- Dark/light mode with Ctrl/⌘+D toggle
- Layout presets: Slim, Standard, Full
- Keyboard shortcuts: F5, F10, F11, Shift+F11
- Auto-reconnect WebSocket
- Console REPL with command history and autocomplete
