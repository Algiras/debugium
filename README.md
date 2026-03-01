# Debugium

**A multi-language debugger with a real-time web UI and LLM integration via MCP.**

Debug Python, JavaScript/TypeScript, and Rust programs from your browser — with AI-driven analysis through the [Model Context Protocol](https://modelcontextprotocol.io/).

[![CI](https://github.com/Algiras/debugium/actions/workflows/ci.yml/badge.svg)](https://github.com/Algiras/debugium/actions/workflows/ci.yml)

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

### macOS / Linux (recommended)

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
