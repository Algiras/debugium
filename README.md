# Debugium

**A multi-language debugger with a real-time web UI and LLM integration via MCP.**

Debug Python, JavaScript/TypeScript, and Rust programs from your browser — with AI-driven analysis through the [Model Context Protocol](https://modelcontextprotocol.io/).

[![CI](https://github.com/Algiras/debugium/actions/workflows/ci.yml/badge.svg)](https://github.com/Algiras/debugium/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-algiras.github.io%2Fdebugium-blue)](https://algiras.github.io/debugium)

![Debugium UI — paused at a breakpoint in cache.py with variables panel showing key, self, and value](docs/screenshot.png)

---

## Features

- **Real-time web UI** — source viewer, breakpoints, variables, call stack, console, timeline, watch expressions, findings
- **Multi-language** — Python (debugpy), Node/TypeScript (js-debug), Rust (lldb-dap)
- **MCP integration** — 40+ tools exposing the full debug session to Claude or any LLM
- **Multi-session** — debug multiple programs simultaneously
- **Execution timeline** — every stop recorded with changed variables and stack summary
- **Watch expressions** — evaluated automatically at every breakpoint
- **Annotations & findings** — LLM (or you) pin notes to source lines and record conclusions
- **Conditional breakpoints** — break only when an expression is true
- **Keyboard shortcuts** — F5 continue, F10 step over, F11 step in, Shift+F11 step out
- **Dark / light mode** toggle
- **Variable search** filter
- **Panel collapse** — resize or hide any panel
- **Auto-reconnect** — UI reconnects to the server after a dropped WebSocket

---

## Install

### Claude Code Plugin (recommended)

```
/plugin marketplace add Algiras/debugium
/plugin install debugium@debugium
```

Then add to your project's `.mcp.json` (see [MCP Tools](#mcp-tools) below).

### macOS / Linux binary

```bash
curl -fsSL https://raw.githubusercontent.com/Algiras/debugium/main/install.sh | bash
```

### From source

```bash
# Prerequisites: Rust stable + wasm-pack
cargo install wasm-pack

# Build UI
wasm-pack build crates/debugium-ui --target web --out-dir pkg
cp crates/debugium-ui/pkg/cm_init.js         crates/debugium-ui/dist/pkg/
cp crates/debugium-ui/pkg/debugium_ui.js      crates/debugium-ui/dist/pkg/
cp crates/debugium-ui/pkg/debugium_ui_bg.wasm crates/debugium-ui/dist/pkg/

# Build & install server
cargo install --path crates/debugium-server
```

---

## Usage

### Debug a Python file

```bash
debugium launch my_script.py --adapter python
```

### Debug a Node.js / TypeScript file

```bash
debugium launch app.js --adapter node
# TypeScript (compiled):
debugium launch dist/app.js --adapter node
```

### Debug a Rust binary

```bash
cargo build
debugium launch target/debug/my_binary --adapter lldb
```

### Set initial breakpoints

```bash
debugium launch my_script.py --adapter python \
  --breakpoint /abs/path/my_script.py:42 \
  --breakpoint /abs/path/helpers.py:15
```

### Enable LLM / MCP integration

Add a `.mcp.json` to your project root (Claude Code picks this up automatically):

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

Then launch the session normally — the MCP server connects to whichever port is active:

```bash
debugium launch my_script.py --adapter python --breakpoint /abs/path/my_script.py:42
```

Claude Code will now have access to all Debugium MCP tools. See [CLAUDE.md](CLAUDE.md) for
the recommended workflow and [SKILL.md](SKILL.md) for the full tool reference.

---

## CLI Control Commands

Once a session is running (`debugium launch …`), you can drive it from a second terminal — or from an LLM agent — without touching the web UI.

Port is auto-discovered from `~/.debugium/port`; override with `--port`.

### Global flags (all subcommands)

| Flag | Default | Description |
|------|---------|-------------|
| `--port PORT` | `~/.debugium/port` | Server port to connect to |
| `--session ID` | `default` | Session to target |
| `--json` | off | Print raw JSON instead of human-readable output |

### Inspection

```bash
debugium sessions                  # list active sessions
debugium threads                   # list threads
debugium stack                     # show call stack
debugium vars                      # show local variables (auto-resolves top frame)
debugium vars --frame-id 2         # show variables for a specific frame
debugium eval "len(fibs)"          # evaluate expression in top frame
debugium eval "x + 1" --frame-id 2
debugium source path/to/file.py    # print full source file
debugium source path/to/file.py --line 43  # windowed ±10 lines with → marker
debugium context                   # full snapshot: paused-at, stack, locals, source, breakpoints
debugium context --compact         # same but truncated (3 frames, 10 vars)
```

### Breakpoints

```bash
debugium bp set FILE:LINE [FILE:LINE …]   # set breakpoints (replaces existing in that file)
debugium bp list                          # list all breakpoints
debugium bp clear                         # clear all breakpoints
```

### Execution control

```bash
debugium continue                  # resume execution
debugium step over                 # step over (next line)
debugium step in                   # step into a function call
debugium step out                  # step out of current function
```

### UI annotations (visible in the web UI)

```bash
debugium annotate FILE:LINE "message" [--color info|warning|error]
debugium finding "message"         [--level  info|warning|error]
```

### Example workflow

```bash
# Terminal A — start the session
debugium launch tests/target_python.py --adapter python \
  --breakpoint "$(pwd)/tests/target_python.py:43"

# Terminal B (or LLM agent) — inspect and drive it
debugium sessions
debugium stack
debugium vars
debugium eval "len(fibs)"
debugium bp set tests/target_python.py:49
debugium continue                  # runs to line 49
debugium vars
debugium step over
debugium context --json            # machine-readable snapshot
debugium annotate tests/target_python.py:43 "called here" --color info
debugium finding "fibs has 10 elements" --level info
debugium bp clear
```

---

## MCP Tools

When connected via MCP, 40+ tools are available. Key ones:

| Category | Tools |
|----------|-------|
| **Orient** | `get_debug_context` ★ (paused location + locals + stack + source in one call) |
| **Breakpoints** | `set_breakpoint`, `set_breakpoints`, `list_breakpoints`, `clear_breakpoints`, `set_function_breakpoints`, `set_exception_breakpoints` |
| **Execution** | `continue_execution`, `step_over`, `step_in`, `step_out`, `pause`, `disconnect` |
| **Inspection** | `get_stack_trace`, `get_scopes`, `get_variables`, `evaluate`, `get_threads`, `get_source` |
| **Output** | `get_console_output`, `wait_for_output` (with `from_line` to avoid stale matches) |
| **History** | `get_timeline`, `get_variable_history` (traces a variable across all stops) |
| **Annotations** | `annotate`, `get_annotations`, `add_finding`, `get_findings` |
| **Watches** | `add_watch`, `remove_watch`, `get_watches` |
| **Compound** | `step_until`, `run_until_exception` |
| **Session** | `get_sessions`, `list_sessions` |

> **Note**: `step_over`, `step_in`, and `step_out` are **blocking** — they wait for the
> adapter to pause before returning. Safe to chain back-to-back without sleeps.
> `continue_execution` returns `console_line_count` for use with `wait_for_output`.

See [SKILL.md](SKILL.md) for the full reference with input schemas.

---

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `F5` | Continue |
| `F10` | Step Over |
| `F11` | Step Into |
| `Shift+F11` | Step Out |
| `Ctrl/⌘+D` | Toggle dark/light mode |

---

## Architecture

```
debugium-server (Axum)
├── DAP proxy — spawns / connects to debug adapters (debugpy, lldb-dap, js-debug)
├── HTTP API  — /state, /dap, /sessions, /annotations, /findings
├── WebSocket — broadcasts DAP events to the UI in real-time
└── MCP stdio — JSON-RPC 2.0 server for LLM tool integration

debugium-ui (Leptos + WASM)
├── CodeMirror 6 — source viewer with breakpoint gutters + exec arrow + LLM annotations
├── Reactive panels — Variables, Stack, Breakpoints, Findings, Watch, Timeline, Console
└── WebSocket client — receives events, sends DAP commands, auto-reconnects
```

---

## Requirements

| Adapter | Requirement |
|---------|-------------|
| Python  | `pip install debugpy` |
| Node.js | `js-debug` (bundled or `npm install -g @vscode/js-debug`) |
| Rust    | `lldb-dap` (installed with `lldb` / Xcode on macOS; `apt install lldb` on Linux) |

---

## License

MIT
