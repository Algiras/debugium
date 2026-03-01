# Debugium Configuration Examples

Each `.dap.json` file is a ready-to-use debug adapter configuration. Copy one to your project root as `dap.json` for auto-discovery, or reference it explicitly with `--config`.

## Usage

```bash
# Explicit config
debugium launch my_program --config examples/python.dap.json --breakpoint /path/to/file.py:42

# Auto-discovery: copy to project root
cp examples/python.dap.json ./dap.json
debugium launch my_program --breakpoint /path/to/file.py:42
```

## Available Configs

| File | Language | Adapter | Mode |
|------|----------|---------|------|
| `python.dap.json` | Python | debugpy | Launch |
| `node.dap.json` | Node.js / JavaScript | js-debug | Launch |
| `typescript.dap.json` | TypeScript | js-debug + tsx | Launch |
| `c-cpp.dap.json` | C / C++ | lldb-dap | Launch |
| `rust.dap.json` | Rust | lldb-dap | Launch |
| `java.dap.json` | Java | microsoft/java-debug | Launch |
| `scala-jvm.dap.json` | Scala | JVM/JDI | Launch |
| `wasm.dap.json` | WebAssembly | lldb-dap | Launch |
| `remote-python.dap.json` | Python (remote) | debugpy over TCP | Remote attach |

## Test targets

The `tests/` directory contains sample programs for each language:

```bash
# Python
debugium launch tests/target_python.py --config examples/python.dap.json \
  --breakpoint "$(pwd)/tests/target_python.py:43"

# Node.js
debugium launch tests/target_node.js --config examples/node.dap.json \
  --breakpoint "$(pwd)/tests/target_node.js:36"

# TypeScript
debugium launch tests/target_ts.ts --config examples/typescript.dap.json \
  --breakpoint "$(pwd)/tests/target_ts.ts:62"

# C (compile first)
cc -g -O0 tests/target_c.c -o /tmp/target_c
debugium launch /tmp/target_c --config examples/c-cpp.dap.json \
  --breakpoint "$(pwd)/tests/target_c.c:22"

# C++ (compile first)
c++ -std=c++17 -g -O0 tests/target_cpp.cpp -o /tmp/target_cpp
debugium launch /tmp/target_cpp --config examples/c-cpp.dap.json \
  --breakpoint "$(pwd)/tests/target_cpp.cpp:32"

# Remote Python
python3 -m debugpy --listen 127.0.0.1:5678 --wait-for-client tests/target_python.py &
debugium launch tests/target_python.py --config examples/remote-python.dap.json \
  --breakpoint "$(pwd)/tests/target_python.py:43"
```

## Customizing

All fields in a `dap.json` are documented in `dap.json.example`. Key optional fields:

- `env` — environment variables merged into launch args
- `args` — CLI arguments for the debuggee
- `stopOnEntry` — pause at the first line
- `justMyCode` — (Python) skip library code
- `skipFiles` — (Node/TS) glob patterns to skip
- `sourceMaps` — (Node/TS) enable source map support
- `pathMappings` — map local paths to remote paths (containers)
- `exceptionBreakpoints` — exception filter IDs (e.g. `["uncaught"]`)
- `breakpoints` — initial breakpoints: `[{ "file": "...", "line": 42 }]`
