# Debugium UI Protocol Coverage Audit

## Scope

- UI surface reviewed: `crates/debugium-ui/src/lib.rs`, `crates/debugium-ui/dist/style.css`
- Server/runtime surface reviewed: `crates/debugium-server/src/server/routes.rs`, `crates/debugium-server/src/mcp/mod.rs`
- Goal: map DAP/MCP capabilities to current UI behavior and rank remaining UX parity gaps.

## Coverage Matrix (DAP/MCP -> UI)

| Capability | Server/MCP | UI Surface | Coverage | Notes |
|---|---|---|---|---|
| Continue / pause / step in/out/over | Yes | Header controls + hotkeys | Full | In-flight/complete feedback is present. |
| Stack trace | Yes | Stack panel | Full | Auto-refresh on stop, frame click navigation. |
| Scopes / variables | Yes | Variables panel | Full | Expansion, inline setVariable, loading state added. |
| Evaluate / REPL | Yes | Console input | Full | Added command history + completions trigger path. |
| Breakpoints (line) | Yes | Source gutter + Breakpoints panel | Full | Added verification feedback pulse. |
| Watches (MCP + evaluate) | Yes | Watch panel | Partial | Add works in UI; remove currently local-only in UI path. |
| Findings / annotations / timeline | Yes | Findings + Timeline + source decorations | Full | Findings and timeline chips now visible. |
| Source read (`get_source`) | Yes | Source panel | Partial | UI highlights MCP source-read context, but no explicit "read mode" affordance. |
| Exception breakpoints | Yes (`raised`,`uncaught`,`userUnhandled`) | Console toggle | Partial | UI only toggles uncaught, no multi-filter selector. |
| Threads list/selection | Yes (`threads`, MCP `get_threads`) | none (stack header only) | Missing | No thread switcher in UI. |
| Function breakpoints | Yes (`set_function_breakpoints`) | none | Missing | No function breakpoint editor/list. |
| Capabilities / exception info | Yes (`get_capabilities`,`get_exception_info`) | none | Missing | No dedicated inspector panel. |
| Advanced DAP (restartFrame/gotoTargets/goto etc.) | Adapter-dependent | none | Missing | No discoverability path from UI. |
| MCP elicitation (form/url, capability-gated) | Implemented server-side | none in web UI | Intentional gap | Supported for MCP clients that advertise capabilities. |

## Ranked Gaps

1. **P0: Thread awareness + active thread selection**
   - Risk: stepping/continuing wrong thread in multi-thread debug sessions.
2. **P0: Exception breakpoint filter parity**
   - Risk: missed crashes/noise due single uncaught toggle instead of full filter set.
3. **P1: Function breakpoint UX**
   - Risk: slower setup in codebases where source line breakpoints are unstable.
4. **P1: Watch lifecycle parity**
   - Risk: remove action drifts from server watch state if managed only in UI list.
5. **P2: Advanced DAP operations (restart frame/goto)**
   - Risk: power-user workflows require MCP/CLI instead of UI.
6. **P2: Capabilities/exception-info inspector**
   - Risk: harder to diagnose adapter feature availability directly in UI.

## Recommendations

- Add a compact thread switcher near the stack panel header; bind all stepping controls to selected thread.
- Replace uncaught-only toggle with multi-select exception filter chips (`raised`, `uncaught`, `userUnhandled`).
- Add function breakpoint mini-form with persistence and verified-state feedback.
- Route watch add/remove through MCP endpoints for strict server/UI parity, then reflect via `watches_*` events.
- Add "Adapter capabilities" drawer to gate advanced controls dynamically.
- Add power actions dropdown (restart frame, goto target) only when capability flags are true.

## Phase 4 Backlog (Prioritized, not implemented)

1. **Thread UX parity** (P0, high ROI)
   - Thread list chip in stack panel
   - Active thread selector
   - Thread-scoped stepping/continue controls
2. **Exception mode expansion** (P0, high ROI)
   - Multi-filter UI for `raised` / `uncaught` / `userUnhandled`
   - Persist per session
3. **Function breakpoints panel** (P1, medium-high ROI)
   - Add/remove function names
   - Verified/error per entry
4. **Watch parity hardening** (P1, medium ROI)
   - Remove watch via MCP path
   - Loading/error state per expression
5. **Advanced DAP controls** (P2, medium ROI)
   - Conditional surfacing by `get_capabilities`
   - `restartFrame`, `gotoTargets/goto`, optional pause-on-start helpers
6. **Capabilities + exception-info inspector** (P2, medium ROI)
   - Read-only diagnostics panel with adapter support matrix
