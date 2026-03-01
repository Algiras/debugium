# Debugium

**A multi-language debugger with a real-time web UI and LLM integration via MCP.**

Debug Python, JavaScript/TypeScript, and Rust programs from your browser — with AI-driven analysis through the [Model Context Protocol](https://modelcontextprotocol.io/).

[![CI](https://github.com/Algiras/debugium/actions/workflows/ci.yml/badge.svg)](https://github.com/Algiras/debugium/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-algiras.github.io%2Fdebugium-blue)](https://algiras.github.io/debugium)

![Debugium UI — paused at a breakpoint in cache.py with variables panel showing key, self, and value](docs/screenshot.png)

---

## Features

- **Real-time web UI** — source viewer, breakpoints, variables, call stack, console
- **Multi-language** — Python (debugpy), Node/TypeScript (js-debug), Rust (lldb-dap)
- **MCP integration** — expose your debug session as tools for Claude or any LLM
- **Multi-session** — debug multiple programs simultaneously
- **Keyboard shortcuts** — F5 continue, F10 step over, F11 step in, Shift+F11 step out
- **Dark / light mode** toggle
- **Variable search** filter
- **Breakpoint list** panel with click-to-navigate

---

## Install

### Claude Code Plugin (recommended)

```
/plugin marketplace add Algiras/debugium
/plugin install debugium@debugium
```

Then add to your project's `.mcp.json` (see [MCP config](#mcp-config) below).

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
debugium launch target/debug/my_binary --adapter rust
```

### Set initial breakpoints

```bash
debugium launch my_script.py --adapter python \
  --breakpoint /abs/path/my_script.py:42 \
  --breakpoint /abs/path/helpers.py:15
```

### Enable LLM / MCP integration

```bash
# Launch with MCP stdio server — pipe into Claude Code
debugium launch my_script.py --adapter python --mcp
```

Or add to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "debugium": {
      "command": "debugium",
      "args": ["mcp", "--port", "7331"]
    }
  }
}
```

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

When connected via MCP, the following tools are available to your LLM:

| Tool | Description |
|------|-------------|
| `get_debug_context` | Paused location, locals, call stack, source window |
| `dap_request` | Send any raw DAP command |
| `set_breakpoint` | Set a breakpoint at file:line |
| `get_console_output` | Read recent program stdout/stderr |
| `list_sessions` | List running debug sessions |
| `annotate` | Add inline gutter annotation in the UI |
| `add_finding` | Post a finding to the Findings panel |
| `step_until` | Step until a condition is true |
| `run_until_exception` | Continue until an exception is raised |

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
├── CodeMirror 6 — source viewer with breakpoint gutters + exec arrow
├── Reactive panels — Variables, Stack, Breakpoints, Findings, Watch, Console
└── WebSocket client — receives events, sends DAP commands
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
