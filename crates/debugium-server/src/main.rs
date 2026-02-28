use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

mod dap;
mod home;
mod mcp;
mod server;

use dap::adapter::{Adapter, AdapterKind};
use dap::session::{Session, SessionRegistry};
use home::DebugiumHome;
use server::hub::Hub;

#[derive(Parser)]
#[command(name = "debugium", about = "Debugium — DAP debugger with real-time web UI + MCP")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch a program under a debug adapter and open the web UI
    Launch {
        /// Path to the program to debug
        program: PathBuf,

        /// Debug adapter type: python, node, lldb
        #[arg(short, long, default_value = "python")]
        adapter: String,

        /// Port for the web UI server (0 = auto-assign a free port)
        #[arg(long, default_value = "0")]
        port: u16,

        /// Start the real-time web UI server
        #[arg(long, default_value_t = true)]
        serve: bool,


        /// Open browser automatically
        #[arg(long)]
        open_browser: bool,

        /// Static assets directory (defaults to crates/debugium-ui/dist)
        #[arg(long)]
        static_dir: Option<PathBuf>,

        /// Initial breakpoints: --breakpoint /abs/path/file.py:42
        #[arg(short, long, value_name = "FILE:LINE")]
        breakpoint: Vec<String>,

        /// Also start the MCP stdio server (for Claude Code / LLM integration)
        #[arg(long)]
        mcp: bool,
    },

    /// Attach to an already-running debug adapter on a TCP port
    Attach {
        #[arg(short, long)]
        port: u16,

        #[arg(long, default_value = "0")]
        serve_port: u16,

        #[arg(long, default_value_t = true)]
        serve: bool,


        #[arg(long)]
        open_browser: bool,

        /// Also start the MCP stdio server
        #[arg(long)]
        mcp: bool,
    },

    /// Start only the MCP stdio server (connects to a running Debugium port)
    Mcp {
        /// Debugium web-server port to connect to
        #[arg(long, default_value = "7331")]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Open (or create) ~/.debugium/
    let home = DebugiumHome::open().unwrap_or_else(|e| {
        eprintln!("Warning: could not open ~/.debugium: {e}");
        // Fall back to a tmp path so startup continues
        DebugiumHome { path: std::env::temp_dir().join("debugium") }
    });

    // Set up logging: stderr + file at ~/.debugium/debugium.log
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true);

    let log_file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(home.log_path())
        .ok();

    let registry = tracing_subscriber::registry()
        .with(EnvFilter::from_default_env().add_directive("debugium=info".parse()?))
        .with(stderr_layer);

    if let Some(file) = log_file {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Arc::new(file))
            .with_ansi(false);
        registry.with(file_layer).init();
    } else {
        registry.init();
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Launch {
            program,
            adapter,
            port,
            serve,
            open_browser,
            static_dir,
            breakpoint,
            mcp,
        } => {
            let hub = Hub::new();
            let registry = SessionRegistry::new();

            let kind = AdapterKind::from_str(&adapter);
            let cwd = std::env::current_dir()?;

            // Metals/TCP-attach creates session differently
            let session = if let AdapterKind::Metals { port: dap_port } = &kind {
                let addr: std::net::SocketAddr = format!("127.0.0.1:{dap_port}").parse()?;
                Session::from_tcp("default".to_string(), addr, Adapter::new(kind.clone()), hub.clone())
                    .await
                    .map_err(|e| { tracing::error!("Failed to connect to Metals: {e}"); e })?
            } else {
                let adapter_obj = Adapter::new(kind);
                Session::new("default".to_string(), adapter_obj, hub.clone())
                    .await
                    .map_err(|e| { tracing::error!("Failed to create session: {e}"); e })?
            };
            registry.insert(session.clone()).await;

            // Parse breakpoints into (file, lines) pairs
            let breakpoints = parse_breakpoints(&breakpoint);

            let static_dir = static_dir.unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/debugium-ui/dist")
            });

            // Start HTTP server; port 0 = OS picks a free one
            let actual_port = if serve {
                server::start_background(hub.clone(), registry.clone(), port, static_dir.clone(), open_browser).await?
            } else {
                port
            };

            home.write_port(actual_port);
            // Print to stderr so both users and LLM tools can discover the URL
            eprintln!("[Debugium] UI ready at http://localhost:{actual_port}");
            eprintln!("[Debugium] Session: default  program: {}", program.display());

            // DAP: proper handshake in background (launch → initialized event → setBreakpoints → configDone)
            let session2 = session.clone();
            let program2 = program.clone();
            let cwd2 = cwd.clone();
            tokio::spawn(async move {
                if let Err(e) = session2.configure_and_launch(program2, cwd2, &breakpoints).await {
                    tracing::error!("configure_and_launch failed: {e}");
                }
            });

            // Optionally run MCP server on stdin/stdout
            if mcp {
                mcp::serve(registry, hub).await?;
            } else {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigterm = signal(SignalKind::terminate()).unwrap();
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = sigterm.recv() => {}
                    }
                }
                #[cfg(not(unix))]
                tokio::signal::ctrl_c().await.ok();
                home.remove_port();
            }
        }

        Commands::Attach { port: _dap_port, serve_port, serve, open_browser, mcp } => {
            let hub = Hub::new();
            let registry = SessionRegistry::new();

            if serve {
                let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../crates/debugium-ui/dist");
                if mcp {
                    let hub2 = hub.clone();
                    let reg2 = registry.clone();
                    tokio::spawn(async move {
                        if let Err(e) = server::start(hub2, reg2, serve_port, static_dir, open_browser).await {
                            tracing::error!("Web server error: {e}");
                        }
                    });
                    mcp::serve(registry, hub).await?;
                } else {
                    server::start(hub, registry, serve_port, static_dir, open_browser).await?;
                }
            }
        }

        Commands::Mcp { port: _port } => {
            // Standalone MCP server — registry is empty but tools still work via HTTP proxy
            let hub = Hub::new();
            let registry = SessionRegistry::new();
            mcp::serve(registry, hub).await?;
        }
    }

    Ok(())
}

fn parse_breakpoints(raw: &[String]) -> Vec<(String, Vec<u32>)> {
    let mut map: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
    for bp in raw {
        if let Some((file, line_str)) = bp.rsplit_once(':') {
            if let Ok(line) = line_str.parse::<u32>() {
                map.entry(file.to_string()).or_default().push(line);
            }
        }
    }
    map.into_iter().collect()
}
