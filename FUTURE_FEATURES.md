# Future Features

Ideas for future Debugium improvements, especially for LLM-driven debugging workflows.

## Exception-First Debugging
- `run_until_exception` already exists; extend it to auto-capture full context snapshot
  (locals, stack, previous N timeline entries) and format it as a structured bug report
  without LLM needing to make follow-up calls.

## Test Integration
- Run `pytest` / `cargo test` / `go test` inside a managed session
- Auto-attach the DAP adapter at the failing assertion line
- Surface test name, expected vs actual, and full diff in the findings panel

## Reverse / Time-Travel Debugging
- Integrate with `rr` (Mozilla Record and Replay) for C/C++/Rust programs
- Add `step_back` and `reverse_continue` MCP tools
- Extend the Timeline panel to allow bidirectional navigation

## Auto Root-Cause Analysis + Patch Proposal
- After an exception or assertion failure, have the LLM summarize the root cause
  using `get_timeline` + `get_debug_context` in a single compound tool call
- Auto-generate a minimal code patch, surface it in a diff panel
- One-click apply with undo support

## Structured Variable Diffs
- `get_timeline` already includes `changed_vars` per entry
- Add a "variable history" view: click a variable → see its value at every timeline step
- Sparkline / heatmap for numeric variables over time

## Multi-Process / Multi-Thread Support
- Track per-thread timelines independently
- Visualize thread interleaving in a timeline swimlane view

## Performance Profiling Integration
- Sample CPU / memory at each `stopped` event
- Add `profiling` field to `TimelineEntry`
- Plot in a flamegraph panel inside the UI
