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
    CodeLldb,
    Custom(Vec<String>),
}

impl AdapterKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "python" | "debugpy" => Self::Python,
            "node" | "pwa-node" | "js" => Self::NodeJs,
            "lldb" | "codelldb" => Self::CodeLldb,
            _ => Self::Python,
        }
    }
}

pub struct Adapter {
    pub kind: AdapterKind,
}

impl Adapter {
    pub fn new(kind: AdapterKind) -> Self {
        Self { kind }
    }

    /// Spawn the debug adapter subprocess.
    pub fn spawn(&self) -> Result<Child> {
        let child = match &self.kind {
            AdapterKind::Python => Command::new("python3")
                .args(["-m", "debugpy.adapter"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,

            AdapterKind::NodeJs => {
                // js-debug dapDebugServer on a random port (piped mode)
                let js_debug_path = which_js_debug();
                Command::new("node")
                    .args([js_debug_path.to_str().unwrap_or(""), "0"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?
            }

            AdapterKind::CodeLldb => Command::new("codelldb")
                .arg("--port=0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,

            AdapterKind::Custom(cmd) => {
                let (prog, args) = cmd.split_first().expect("empty command");
                Command::new(prog)
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()?
            }
        };

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

            AdapterKind::CodeLldb => json!({
                "type": "lldb",
                "request": "launch",
                "program": program.to_str().unwrap_or(""),
                "cwd": cwd.to_str().unwrap_or(""),
                "args": []
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
            AdapterKind::CodeLldb => "lldb",
            AdapterKind::Custom(_) => "custom",
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
