# DAP Protocol Coverage Audit — Debugium

This document compares Debugium's implementation against the full DAP (Debug Adapter Protocol) specification. Sources: `mcp/mod.rs`, `mcp/tools.rs`, `dap/session.rs`, `dap/client.rs`, `server/routes.rs`.

---

## A) DAP Requests We DO Support

| DAP Request | Where Used | Notes |
|-------------|------------|-------|
| **initialize** | `session.rs` (Session::new, from_tcp, attach_child_session, attach_debugpy_child) | Handshake; capabilities stored |
| **launch** | `session.rs` (configure_and_launch, attach_child_session) | Fire-and-forget before configurationDone |
| **attach** | `session.rs` (configure_and_attach, attach_debugpy_child) | Remote attach mode |
| **disconnect** | `tools.rs` (disconnect, stop_session) | With `terminateDebuggee` |
| **terminate** | `tools.rs` (terminate) | Graceful process end; requires `supportsTerminateRequest` |
| **restart** | `tools.rs` (restart) | Via `notify`; requires `supportsRestartRequest` |
| **setBreakpoints** | `session.rs`, `tools.rs`, `routes.rs` | Full condition/hitCondition/logMessage support |
| **setExceptionBreakpoints** | `session.rs`, `tools.rs`, `routes.rs` | filters: raised, uncaught, userUnhandled |
| **setFunctionBreakpoints** | `tools.rs` (set_function_breakpoints) | Requires `supportsFunctionBreakpoints` |
| **dataBreakpointInfo** | `tools.rs` (set_data_breakpoint) | Queries adapter before setDataBreakpoints |
| **setDataBreakpoints** | `tools.rs` (set_data_breakpoint, clear_data_breakpoints) | Requires `supportsDataBreakpoints` |
| **continue** | `tools.rs`, `routes.rs` | continue_execution, continue_until, runToCursor |
| **next** | `tools.rs` (step_over, step_until, step_until_change) | Step over |
| **stepIn** | `tools.rs` (step_in) | Step into |
| **stepOut** | `tools.rs` (step_out) | Step out |
| **pause** | `tools.rs` (pause) | Pause running thread |
| **stackTrace** | `tools.rs`, `session.rs` (enrich_stopped) | Full support |
| **scopes** | `tools.rs`, `session.rs` (enrich_stopped) | Full support |
| **variables** | `tools.rs`, `session.rs` (enrich_stopped) | Full support |
| **evaluate** | `tools.rs`, `routes.rs`, `session.rs` (watches) | Full support |
| **setVariable** | `tools.rs`, `routes.rs` | Requires `supportsSetVariable` |
| **exceptionInfo** | `tools.rs`, `session.rs` (enrich_stopped) | Requires `supportsExceptionInfoRequest` |
| **restartFrame** | `tools.rs` (restart_frame) | Requires `supportsRestartFrame` |
| **readMemory** | `tools.rs` (read_memory) | Requires `supportsReadMemoryRequest` |
| **writeMemory** | `tools.rs` (write_memory) | Requires `supportsWriteMemoryRequest` |
| **disassemble** | `tools.rs` (disassemble) | Requires `supportsDisassembleRequest` |
| **configurationDone** | `session.rs` | Part of launch/attach handshake |
| **completions** | `routes.rs` (WebSocket) | UI-driven; passthrough to adapter |
| **breakpointLocations** | `tools.rs` (breakpoint_locations) | Requires `supportsBreakpointLocationsRequest`; valid breakpoint lines in range |
| **stepInTargets** | `tools.rs` (step_in_targets) | Requires `supportsStepInTargetsRequest`; list step-in targets |
| **setExpression** | `tools.rs` (set_expression) | Requires `supportsSetExpression`; set value via expression |
| **loadedSources** | `tools.rs` (loaded_sources) | Requires `supportsLoadedSourcesRequest`; list all loaded files |
| **source** | `tools.rs` (source_by_reference) | Fetch code by sourceReference (for generated/internal code) |
| **gotoTargets** | `tools.rs` (goto_targets) | Requires `supportsGotoTargetsRequest`; query valid jump targets |
| **goto** | `tools.rs` (goto) | Requires `supportsGotoTargetsRequest`; jump execution to a target |
| **cancel** | `tools.rs` (cancel_request) | Requires `supportsCancelRequest`; abort in-flight requests |

---

## B) DAP Requests We DON'T Support

| DAP Request | Priority | Adapter Support | Notes |
|-------------|----------|-----------------|-------|
| **setInstructionBreakpoints** | Low | Rare (native debuggers) | Requires `supportsInstructionBreakpoints`; LLDB, GDB only |
| **stepBack** | Low | Rare | Time-travel debugging; requires `supportsStepBack` |
| **reverseContinue** | Low | Rare | Time-travel; requires `supportsStepBack` |
| **terminateThreads** | Low | Uncommon | Kill specific threads; requires `supportsTerminateThreadsRequest` |
| **modules** | Low | Uncommon | List loaded modules; requires `supportsModulesRequest` |

---

## C) DAP Events — Handled vs Not Handled

### Events We Handle (with special logic)

| Event | Handling |
|-------|----------|
| **initialized** | Intercepted; triggers handshake completion; not broadcast |
| **stopped** | Stored in `last_stopped`; triggers `enrich_stopped` (threads, stackTrace, scopes, variables, timeline, watches, exceptionInfo); broadcast |
| **continued** | Clears `last_stopped`; broadcast |
| **output** | Appended to `console_lines` buffer (last 500); broadcast |
| **exited** | Sets `terminated_tx`; broadcast |
| **terminated** | Sets `terminated_tx`; broadcast |

### Events We Forward (no special logic)

All other DAP events are forwarded to WebSocket clients via `broadcast_json`. These include:

| Event | Notes |
|-------|-------|
| **thread** | Forwarded; no special handling |
| **breakpoint** | Forwarded |
| **module** | Forwarded |
| **loadedSource** | Forwarded |
| **process** | Forwarded |
| **capabilities** | Not a standard DAP event; capabilities come from `initialize` response |
| **memory** | Forwarded |
| **invalidated** | Forwarded |
| **progressStart** | Forwarded |
| **progressUpdate** | Forwarded |
| **progressEnd** | Forwarded |

### Synthetic Events (Debugium-specific)

| Event | Source |
|-------|--------|
| **breakpoints_changed** | MCP tools (set/clear breakpoints) |
| **commandSent** | MCP tools (broadcast command name) |
| **llmQuery** | MCP tools (LLM activity) |
| **annotation_added** | MCP annotate tool |
| **finding_added** | MCP add_finding tool |
| **watches_list_changed** | MCP add_watch / remove_watch |
| **watches_updated** | enrich_stopped (watch evaluation results) |
| **timeline_entry** | enrich_stopped (per-stop snapshot) |
| **exceptionInfo** | enrich_stopped (synthetic when reason=exception) |
| **sourceLoaded** | enrich_stopped (local file read) |
| **session_launched** | launch_session, POST /sessions |

---

## D) DAP Reverse Requests (Adapter → Client)

| Reverse Request | Handling | Notes |
|-----------------|----------|-------|
| **startDebugging** | ✅ Handled | Opens child DAP session (js-debug, debugpy); forwards config to `attach_child_session` / `attach_debugpy_child` |
| **runInTerminal** | ⚠️ Ack only | Client responds `success: true` but does **not** run a terminal command; adapter may assume it ran |

---

## Summary Tables

### By Priority (Missing Items)

| Priority | Requests | Events | Reverse |
|----------|----------|--------|---------|
| **High** | — | — | runInTerminal (partial) |
| **Medium** | — | — | — |
| **Low** | setInstructionBreakpoints, stepBack, reverseContinue, terminateThreads, modules | — | — |

### Capability-Gated MCP Tools

These tools are only exposed when the adapter declares the corresponding capability:

| MCP Tool | Capability |
|----------|------------|
| read_memory | supportsReadMemoryRequest |
| write_memory | supportsWriteMemoryRequest |
| disassemble | supportsDisassembleRequest |
| set_function_breakpoints | supportsFunctionBreakpoints |
| set_variable | supportsSetVariable |
| restart | supportsRestartRequest |
| terminate | supportsTerminateRequest |
| get_exception_info | supportsExceptionInfoRequest |
| restart_frame | supportsRestartFrame |
| set_expression | supportsSetExpression |
| breakpoint_locations | supportsBreakpointLocationsRequest |
| step_in_targets | supportsStepInTargetsRequest |
| loaded_sources | supportsLoadedSourcesRequest |
| goto_targets | supportsGotoTargetsRequest |
| goto | supportsGotoTargetsRequest |
| cancel_request | supportsCancelRequest |

---

## Recommendations

1. **runInTerminal**: Implement actual terminal execution or document that Debugium does not support it (adapters may spawn processes expecting a real terminal).
2. **modules**: Consider adding if needed for DLL/module inspection use cases.
3. **stepBack / reverseContinue**: Only worth implementing if time-travel adapters (rr, UDB) become common.
