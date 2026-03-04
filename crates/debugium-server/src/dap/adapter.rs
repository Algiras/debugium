use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::process::{Child, Command};

/// Configuration loaded from a `dap.json` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DapConfig {
    /// Adapter executable + arguments. None for remote attach mode.
    pub command: Option<Vec<String>>,
    /// Launch arguments forwarded verbatim to the adapter.
    pub launch: Value,
    /// Attach-mode arguments (alternative to launch).
    pub attach: Option<Value>,
    /// "launch" or "attach" (default "launch").
    pub request: String,
    /// Adapter type identifier used in `initialize`.
    pub adapter_id: String,
    /// Whether this adapter speaks TCP after spawn (like js-debug). Default false.
    pub tcp_after_spawn: bool,
    /// Remote DAP server host (for attach-to-running-server mode).
    pub host: Option<String>,
    /// Remote DAP server port.
    pub port: Option<u16>,
    /// Environment variables merged into launch/attach args.
    pub env: Option<Value>,
    /// Debuggee CLI arguments.
    pub args: Option<Vec<String>>,
    /// Human-readable config name.
    pub name: Option<String>,
    /// Stop on entry.
    pub stop_on_entry: Option<bool>,
    /// Python: just my code.
    pub just_my_code: Option<bool>,
    /// Node/TS: file patterns to skip.
    pub skip_files: Option<Vec<String>>,
    /// Node/TS: enable source maps.
    pub source_maps: Option<bool>,
    /// Local↔remote path mapping for containers.
    pub path_mappings: Option<Value>,
    /// Exception breakpoint filter IDs e.g. ["uncaught"].
    pub exception_breakpoints: Option<Vec<String>>,
    /// Initial breakpoints: [{file, line, condition?}].
    pub breakpoints: Option<Vec<Value>>,
}

/// Helper: try camelCase key first, then snake_case.
fn get_field<'a>(v: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| v.get(snake))
}

impl DapConfig {
    /// Load a single adapter config from a JSON object value.
    pub fn from_value(v: &Value) -> Result<Self> {
        let command: Option<Vec<String>> = v.get("command")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(|s| s.as_str().map(str::to_string)).collect());

        let host = v.get("host").and_then(Value::as_str).map(str::to_string);
        let port = v.get("port").and_then(Value::as_u64).map(|p| p as u16);

        // Validate: either command or host+port must be present
        if command.is_none() && (host.is_none() || port.is_none()) {
            anyhow::bail!("dap.json: must provide either \"command\" or both \"host\" and \"port\"");
        }
        if let Some(ref cmd) = command {
            if cmd.is_empty() {
                anyhow::bail!("dap.json: \"command\" array is empty");
            }
        }

        let launch = v.get("launch").cloned().unwrap_or(Value::Null);
        let attach = v.get("attach").cloned();

        // Default request to "attach" if host+port present or attach block exists, else "launch"
        let request = v.get("request").and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                if host.is_some() || attach.is_some() { "attach".to_string() } else { "launch".to_string() }
            });

        let adapter_id = get_field(&v, "adapterId", "adapter_id")
            .and_then(Value::as_str)
            .unwrap_or("custom")
            .to_string();

        let tcp_after_spawn = get_field(&v, "tcpAfterSpawn", "tcp_after_spawn")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let env = v.get("env").cloned();

        let args = v.get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect());

        let name = v.get("name").and_then(Value::as_str).map(str::to_string);

        let stop_on_entry = get_field(&v, "stopOnEntry", "stop_on_entry")
            .and_then(Value::as_bool);

        let just_my_code = get_field(&v, "justMyCode", "just_my_code")
            .and_then(Value::as_bool);

        let skip_files = get_field(&v, "skipFiles", "skip_files")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect());

        let source_maps = get_field(&v, "sourceMaps", "source_maps")
            .and_then(Value::as_bool);

        let path_mappings = get_field(&v, "pathMappings", "path_mappings").cloned();

        let exception_breakpoints = get_field(&v, "exceptionBreakpoints", "exception_breakpoints")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect());

        let breakpoints = v.get("breakpoints")
            .and_then(Value::as_array)
            .cloned();

        Ok(DapConfig {
            command, launch, attach, request, adapter_id, tcp_after_spawn,
            host, port, env, args, name, stop_on_entry, just_my_code,
            skip_files, source_maps, path_mappings, exception_breakpoints, breakpoints,
        })
    }

    /// Load from a JSON file path (single-object format).
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read dap.json at {}: {e}", path.display()))?;
        let v: Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("invalid dap.json: {e}"))?;
        Self::from_value(&v)
    }

    /// Whether this config uses remote TCP attach (host+port, no local spawn).
    pub fn is_remote_attach(&self) -> bool {
        self.host.is_some() && self.port.is_some()
    }

    /// TCP host for remote attach, defaults to 127.0.0.1.
    pub fn tcp_host(&self) -> &str {
        self.host.as_deref().unwrap_or("127.0.0.1")
    }
}

/// Multiple adapter configs from a single dap.json (array format).
/// Each entry has a `files` glob list for auto-matching by program path.
pub struct DapMultiConfig {
    pub entries: Vec<(Vec<String>, DapConfig)>,
}

impl DapMultiConfig {
    /// Load from a JSON file. Accepts both array (multi) and object (single) formats.
    /// Returns Err only on I/O or parse failure; returns Ok(None) for single-object files.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read dap.json at {}: {e}", path.display()))?;
        let v: Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("invalid dap.json: {e}"))?;

        let arr = match v.as_array() {
            Some(a) => a,
            None => return Ok(None), // single-object format
        };

        let mut entries = Vec::new();
        for item in arr {
            let globs: Vec<String> = item.get("files")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let cfg = DapConfig::from_value(item)?;
            entries.push((globs, cfg));
        }
        Ok(Some(Self { entries }))
    }

    /// Find the first config whose `files` globs match the given program filename.
    pub fn match_program(&self, program: &Path) -> Option<&DapConfig> {
        let filename = program.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        for (globs, cfg) in &self.entries {
            for pattern in globs {
                if let Ok(g) = glob::Pattern::new(pattern) {
                    if g.matches(filename) {
                        return Some(cfg);
                    }
                }
            }
        }
        None
    }
}

/// Infer an AdapterKind from the program file extension alone (no config file).
pub fn adapter_kind_from_extension(program: &Path) -> Option<AdapterKind> {
    let ext = program.extension().and_then(|e| e.to_str())?;
    match ext {
        "py" => Some(AdapterKind::Python),
        "js" | "mjs" | "cjs" => Some(AdapterKind::NodeJs),
        "ts" | "tsx" => Some(AdapterKind::TypeScript),
        "rs" => Some(AdapterKind::CodeLldb),
        "c" | "cpp" | "cc" | "cxx" => Some(AdapterKind::CodeLldb),
        "java" => Some(AdapterKind::Java),
        "scala" | "sc" => Some(AdapterKind::Metals { port: 5005 }),
        "wasm" => Some(AdapterKind::Wasm),
        _ => None,
    }
}

/// The type of debug adapter to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterKind {
    Python,
    NodeJs,
    /// TypeScript via js-debug with ts-node/tsx runtime
    TypeScript,
    /// Native code via lldb-dap / codelldb
    CodeLldb,
    /// Java programs via microsoft/java-debug vscode adapter
    Java,
    /// Scala via Metals Language Server DAP (attach mode via TCP)
    Metals { port: u16 },
    /// WebAssembly via lldb-dap with WASM target support
    Wasm,
    /// Fully custom adapter loaded from a dap.json config file.
    DapConfig(DapConfig),
    Custom(Vec<String>),
}

impl AdapterKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "python" | "debugpy" => Self::Python,
            "node" | "pwa-node" | "js" => Self::NodeJs,
            "typescript" | "ts" | "ts-node" | "tsx" => Self::TypeScript,
            "lldb" | "codelldb" | "rust" => Self::CodeLldb,
            "java" | "jvm" => Self::Java,
            "wasm" | "webassembly" => Self::Wasm,
            "metals" | "scala" => Self::Metals { port: 5005 },
            _ if s.starts_with("metals:") => {
                let port = s.trim_start_matches("metals:").parse().unwrap_or(5005);
                Self::Metals { port }
            }
            // dap.json — resolve relative to cwd
            _ if s == "dap.json" || s.ends_with(".json") || s.starts_with("config:") => {
                let path_str = s.trim_start_matches("config:");
                let path = PathBuf::from(path_str);
                match DapConfig::load(&path) {
                    Ok(cfg) => Self::DapConfig(cfg),
                    Err(e) => {
                        eprintln!("Warning: failed to load dap config {path_str}: {e}");
                        Self::Custom(vec![path_str.to_string()])
                    }
                }
            }
            _ => Self::Python,
        }
    }
}

/// Information about a spawned adapter process.
pub struct AdapterProcess {
    pub pid: u32,
    #[allow(dead_code)]
    pub argv: String,
}

pub struct Adapter {
    pub kind: AdapterKind,
    /// Populated after `spawn()` is called.
    pub process: Option<AdapterProcess>,
}

impl Adapter {
    pub fn new(kind: AdapterKind) -> Self {
        Self { kind, process: None }
    }

    /// Spawn the debug adapter subprocess.
    pub fn spawn(&mut self) -> Result<Child> {
        let (child, argv) = match &self.kind {
            AdapterKind::Python => {
                let child = Command::new("python3")
                    .args(["-m", "debugpy.adapter"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = "python3 -m debugpy.adapter".to_string();
                (child, argv)
            }

            AdapterKind::NodeJs => {
                // js-debug dapDebugServer on a random port (piped mode)
                let js_debug_path = which_js_debug();
                let js_str = js_debug_path.to_str().unwrap_or("").to_string();
                let child = Command::new("node")
                    .args([&js_str, "0"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = format!("node {} 0", js_str);
                (child, argv)
            }

            AdapterKind::TypeScript => {
                // TypeScript uses the same js-debug adapter as Node.js
                let js_debug_path = which_js_debug();
                let js_str = js_debug_path.to_str().unwrap_or("").to_string();
                let child = Command::new("node")
                    .args([&js_str, "0"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = format!("node {} 0", js_str);
                (child, argv)
            }

            AdapterKind::Java => {
                // Launch Microsoft java-debug-adapter (requires the JAR on PATH or in standard locations)
                let jar = find_java_debug_jar();
                let jar_str = jar.to_str().unwrap_or("").to_string();
                let child = Command::new("java")
                    .args(["-jar", &jar_str, "0"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = format!("java -jar {} 0", jar_str);
                (child, argv)
            }

            AdapterKind::Metals { .. } => {
                // Metals DAP server is already running; we'll connect via TCP in session.rs.
                // Return a dummy child that exits immediately (connection handled separately).
                anyhow::bail!("Metals adapter uses TCP attach mode — use Session::attach_tcp() instead");
            }

            AdapterKind::CodeLldb => {
                let lldb_path = find_lldb_dap();
                let lldb_str = lldb_path.to_str().unwrap_or("lldb-dap").to_string();
                let child = Command::new(&lldb_path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = lldb_str;
                (child, argv)
            }

            AdapterKind::Wasm => {
                // WASM debugging via lldb-dap (LLVM ≥16 has basic WASM support)
                let lldb_path = find_lldb_dap();
                let lldb_str = lldb_path.to_str().unwrap_or("lldb-dap").to_string();
                let child = Command::new(&lldb_path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = format!("{lldb_str} (wasm)");
                (child, argv)
            }

            AdapterKind::DapConfig(cfg) => {
                let cmd = cfg.command.as_ref()
                    .ok_or_else(|| anyhow::anyhow!("DapConfig has no command — use TCP attach mode instead"))?;
                let (prog, args) = cmd.split_first().expect("empty dap.json command");
                let child = Command::new(prog)
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = cmd.join(" ");
                (child, argv)
            }

            AdapterKind::Custom(cmd) => {
                let (prog, args) = cmd.split_first().expect("empty command");
                let child = Command::new(prog)
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?;
                let argv = cmd.join(" ");
                (child, argv)
            }
        };

        let pid = child.id().unwrap_or(0);
        self.process = Some(AdapterProcess { pid, argv });

        Ok(child)
    }

    /// Build the `launch` arguments for the given program path.
    pub fn launch_args(&self, program: &Path, cwd: &Path) -> Value {
        match &self.kind {
            AdapterKind::Python => json!({
                "type": "python",
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "console": "internalConsole",
                "cwd": cwd.to_str().unwrap_or(""),
                "justMyCode": false,
                "subProcess": true,
                "debugOptions": ["RedirectOutput", "ShowReturnValue"]
            }),

            AdapterKind::NodeJs => json!({
                "type": "pwa-node",
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or(""),
                "console": "internalConsole",
                "skipFiles": ["<node_internals>/**"]
            }),

            AdapterKind::TypeScript => {
                let runtime = if which_cmd("tsx") { "tsx" } else { "node" };
                json!({
                    "type": "pwa-node",
                    "request": "launch",
                    "program": program.to_str().unwrap_or(""),
                    "cwd": cwd.to_str().unwrap_or(""),
                    "console": "internalConsole",
                    "skipFiles": ["<node_internals>/**"],
                    "runtimeExecutable": runtime,
                    "runtimeArgs": [],
                    "stopOnEntry": false,
                    "sourceMaps": true,
                    "outFiles": [],
                    "pauseForSourceMap": false,
                    "smartStep": false,
                })
            }

            AdapterKind::CodeLldb => json!({
                "type": "lldb",
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or(""),
                "args": []
            }),

            AdapterKind::Java => json!({
                "type": "java",
                "request": "launch",
                "mainClass": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or(""),
                "console": "internalConsole",
            }),

            AdapterKind::Metals { port } => json!({
                "type": "scala",
                "request": "attach",
                "hostName": "localhost",
                "port": port,
                "buildTarget": program.file_stem()
                    .and_then(|s| s.to_str()).unwrap_or("root"),
            }),

            AdapterKind::Wasm => json!({
                "type": "lldb",
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or(""),
                "args": [],
                "env": {},
                // WASM debugging requires the source map and dwarf info embedded in the .wasm
                "sourceLanguages": ["webassembly", "rust", "c", "cpp"],
            }),

            AdapterKind::DapConfig(cfg) => {
                // For attach mode, use the attach block; otherwise use launch block
                let base = if cfg.request == "attach" {
                    cfg.attach.clone().unwrap_or(Value::Null)
                } else {
                    cfg.launch.clone()
                };

                let mut args = if base.is_null() {
                    json!({
                        "request": cfg.request,
                        "program": program.to_str().unwrap_or(""),
                        "cwd": cwd.to_str().unwrap_or("")
                    })
                } else {
                    let mut args = base;
                    if let Some(obj) = args.as_object_mut() {
                        obj.entry("request").or_insert_with(|| json!(&cfg.request));
                        obj.entry("program").or_insert_with(|| json!(program.to_str().unwrap_or("")));
                        obj.entry("cwd").or_insert_with(|| json!(cwd.to_str().unwrap_or("")));
                    }
                    args
                };

                // Merge optional DapConfig fields into the args object
                if let Some(obj) = args.as_object_mut() {
                    if let Some(ref env) = cfg.env {
                        obj.entry("env").or_insert_with(|| env.clone());
                    }
                    if let Some(ref cli_args) = cfg.args {
                        obj.entry("args").or_insert_with(|| json!(cli_args));
                    }
                    if let Some(v) = cfg.stop_on_entry {
                        obj.entry("stopOnEntry").or_insert_with(|| json!(v));
                    }
                    if let Some(v) = cfg.just_my_code {
                        obj.entry("justMyCode").or_insert_with(|| json!(v));
                    }
                    if let Some(ref v) = cfg.skip_files {
                        obj.entry("skipFiles").or_insert_with(|| json!(v));
                    }
                    if let Some(v) = cfg.source_maps {
                        obj.entry("sourceMaps").or_insert_with(|| json!(v));
                    }
                    if let Some(ref v) = cfg.path_mappings {
                        obj.entry("pathMappings").or_insert_with(|| v.clone());
                    }
                }

                // Expand ${program} and ${cwd} placeholders in all string values
                let prog_str = program.to_str().unwrap_or("");
                let cwd_str = cwd.to_str().unwrap_or("");
                expand_placeholders(&mut args, prog_str, cwd_str);

                args
            }

            AdapterKind::Custom(_) => json!({
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or("")
            }),
        }
    }

    /// Build the `attach` arguments for connecting to a remote debug target.
    pub fn attach_args(&self, host: &str, port: u16, program: &Path, cwd: &Path) -> Value {
        match &self.kind {
            AdapterKind::Python => json!({
                "type": "python",
                "request": "attach",
                "connect": { "host": host, "port": port },
                "justMyCode": false,
                "subProcess": true,
                "debugOptions": ["RedirectOutput", "ShowReturnValue"],
                "pathMappings": [{
                    "localRoot": cwd.to_str().unwrap_or(""),
                    "remoteRoot": "."
                }]
            }),

            AdapterKind::NodeJs | AdapterKind::TypeScript => json!({
                "type": "pwa-node",
                "request": "attach",
                "address": host,
                "port": port,
                "skipFiles": ["<node_internals>/**"],
                "sourceMaps": true
            }),

            AdapterKind::Java => json!({
                "type": "java",
                "request": "attach",
                "hostName": host,
                "port": port
            }),

            AdapterKind::CodeLldb => json!({
                "type": "lldb",
                "request": "attach",
                "connectRemote": format!("connect://{}:{}", host, port),
                "program": program.to_str().unwrap_or("")
            }),

            AdapterKind::Metals { .. } => json!({
                "type": "scala",
                "request": "attach",
                "hostName": host,
                "port": port,
                "buildTarget": program.file_stem()
                    .and_then(|s| s.to_str()).unwrap_or("root")
            }),

            AdapterKind::DapConfig(cfg) => {
                // Start with the attach block if present, else build a minimal one
                let mut args = cfg.attach.clone().unwrap_or_else(|| json!({
                    "request": "attach"
                }));
                if let Some(obj) = args.as_object_mut() {
                    obj.entry("request").or_insert_with(|| json!("attach"));
                    // Merge host/port — adapter-specific key names vary,
                    // so insert both common patterns
                    obj.entry("host").or_insert_with(|| json!(host));
                    obj.entry("hostName").or_insert_with(|| json!(host));
                    obj.entry("port").or_insert_with(|| json!(port));
                    obj.entry("program").or_insert_with(|| json!(program.to_str().unwrap_or("")));
                    obj.entry("cwd").or_insert_with(|| json!(cwd.to_str().unwrap_or("")));
                }
                let prog_str = program.to_str().unwrap_or("");
                let cwd_str = cwd.to_str().unwrap_or("");
                expand_placeholders(&mut args, prog_str, cwd_str);
                args
            }

            AdapterKind::Wasm | AdapterKind::Custom(_) => json!({
                "request": "attach",
                "program": program.to_str().unwrap_or(""),
                "host": host,
                "port": port
            }),
        }
    }

    /// Adapter type string used in `initialize` request.
    pub fn adapter_id(&self) -> &str {
        match &self.kind {
            AdapterKind::Python => "debugpy",
            AdapterKind::NodeJs => "pwa-node",
            AdapterKind::TypeScript => "pwa-node",
            AdapterKind::CodeLldb => "lldb",
            AdapterKind::Java => "java",
            AdapterKind::Wasm => "lldb",
            AdapterKind::Metals { .. } => "metals",
            AdapterKind::DapConfig(cfg) => &cfg.adapter_id,
            AdapterKind::Custom(_) => "custom",
        }
    }

    /// Whether this adapter connects via TCP rather than spawning a subprocess.
    pub fn is_tcp_attach(&self) -> bool {
        match &self.kind {
            AdapterKind::Metals { .. } => true,
            AdapterKind::DapConfig(cfg) => cfg.is_remote_attach(),
            _ => false,
        }
    }

    /// TCP host for attach-mode adapters (defaults to 127.0.0.1).
    pub fn tcp_host(&self) -> &str {
        match &self.kind {
            AdapterKind::DapConfig(cfg) => cfg.tcp_host(),
            _ => "127.0.0.1",
        }
    }

    /// True for adapters that spawn a TCP server and print the port to stdout
    /// (js-debug for Node.js / TypeScript).
    pub fn is_tcp_after_spawn(&self) -> bool {
        match &self.kind {
            AdapterKind::NodeJs | AdapterKind::TypeScript => true,
            AdapterKind::DapConfig(cfg) => cfg.tcp_after_spawn,
            _ => false,
        }
    }

    /// TCP port for attach-mode adapters.
    pub fn tcp_port(&self) -> Option<u16> {
        match &self.kind {
            AdapterKind::Metals { port } => Some(*port),
            AdapterKind::DapConfig(cfg) => cfg.port,
            _ => None,
        }
    }
}

fn which_js_debug() -> PathBuf {
    // Look for the bundled js-debug adapter
    let candidates = [
        "./js-debug/js-debug/src/dapDebugServer.js",
        "/usr/local/lib/js-debug/src/dapDebugServer.js",
    ];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("dapDebugServer.js")
}

fn find_lldb_dap() -> PathBuf {
    let candidates = [
        "/opt/homebrew/opt/llvm/bin/lldb-dap",
        "/opt/homebrew/bin/lldb-dap",
        "/opt/homebrew/opt/llvm@16/bin/lldb-vscode",
        "/usr/local/bin/lldb-dap",
        "lldb-dap",
        "lldb-vscode",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() || which_cmd(c) {
            return p;
        }
    }
    PathBuf::from("lldb-dap")
}

fn find_java_debug_jar() -> PathBuf {
    let candidates = [
        // VS Code extension install locations
        "~/.vscode/extensions/vscjava.vscode-java-debug-*/server/com.microsoft.java.debug.plugin-*.jar",
        "./java-debug/com.microsoft.java.debug.plugin.jar",
        "java-debug-adapter.jar",
    ];
    for pattern in &candidates {
        // Expand home dir
        let expanded = pattern.replacen("~", &std::env::var("HOME").unwrap_or_default(), 1);
        // Use glob to find actual file
        if let Ok(mut paths) = glob::glob(&expanded) {
            if let Some(Ok(p)) = paths.next() {
                return p;
            }
        }
    }
    PathBuf::from("java-debug-adapter.jar")
}

/// Replace `${program}` and `${cwd}` placeholders in all JSON string values.
fn expand_placeholders(value: &mut Value, program: &str, cwd: &str) {
    match value {
        Value::String(s) => {
            if s.contains("${program}") || s.contains("${cwd}") {
                *s = s.replace("${program}", program).replace("${cwd}", cwd);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                expand_placeholders(item, program, cwd);
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                expand_placeholders(v, program, cwd);
            }
        }
        _ => {}
    }
}

fn which_cmd(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
