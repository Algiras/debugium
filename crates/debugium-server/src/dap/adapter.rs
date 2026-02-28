use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::process::{Child, Command};

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
            "metals" | "scala" => Self::Metals { port: 5005 },
            _ if s.starts_with("metals:") => {
                let port = s.trim_start_matches("metals:").parse().unwrap_or(5005);
                Self::Metals { port }
            }
            _ => Self::Python,
        }
    }
}

/// Information about a spawned adapter process.
pub struct AdapterProcess {
    pub pid: u32,
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

            AdapterKind::Custom(_) => json!({
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or("")
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
            AdapterKind::Metals { .. } => "metals",
            AdapterKind::Custom(_) => "custom",
        }
    }

    /// Whether this adapter connects via TCP rather than spawning a subprocess.
    pub fn is_tcp_attach(&self) -> bool {
        matches!(self.kind, AdapterKind::Metals { .. })
    }

    /// True for adapters that spawn a TCP server and print the port to stdout
    /// (js-debug for Node.js / TypeScript).
    pub fn is_tcp_after_spawn(&self) -> bool {
        matches!(self.kind, AdapterKind::NodeJs | AdapterKind::TypeScript)
    }

    /// TCP port for attach-mode adapters.
    pub fn tcp_port(&self) -> Option<u16> {
        match &self.kind {
            AdapterKind::Metals { port } => Some(*port),
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

fn which_cmd(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
