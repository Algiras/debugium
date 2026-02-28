use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod dap;
mod server;

use dap::adapter::{Adapter, AdapterKind};
use dap::session::{Session, SessionRegistry};
use server::hub::Hub;

#[derive(Parser)]
#[command(name = "debugium", about = "Debugium — DAP debugger with real-time web UI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch a program and debug it
    Launch {
        /// Path to the program to debug
        program: PathBuf,

        /// Debug adapter type (python, node, lldb)
        #[arg(short, long, default_value = "python")]
        adapter: String,

        /// Port for the web UI server
        #[arg(long, default_value = "7331")]
        port: u16,

        /// Serve the real-time debugger web UI
        #[arg(long)]
        serve: bool,

        /// Open browser automatically
        #[arg(long)]
        open_browser: bool,

        /// Static assets directory (defaults to crates/debugium-ui/dist)
        #[arg(long)]
        static_dir: Option<PathBuf>,

        /// Breakpoints to set on launch (format: file:line)
        #[arg(short, long, value_name = "FILE:LINE")]
        breakpoint: Vec<String>,
    },

    /// Attach to an existing debug adapter on a port
    Attach {
        #[arg(short, long)]
        port: u16,

        #[arg(long, default_value = "7331")]
        serve_port: u16,

        #[arg(long)]
        serve: bool,

        #[arg(long)]
        open_browser: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("debugium=info".parse()?))
        .init();

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
        } => {
            let hub = Hub::new();
            let registry = SessionRegistry::new();
            let session_id = "default".to_string();

            let kind = AdapterKind::from_str(&adapter);
            let adapter = Adapter::new(kind);
            let cwd = std::env::current_dir()?;

            let session = Session::new(session_id.clone(), adapter, hub.clone()).await?;

            // Set breakpoints before config_done
            for bp_str in &breakpoint {
                if let Some((file, line_str)) = bp_str.split_once(':') {
                    if let Ok(line) = line_str.parse::<u32>() {
                        session.set_breakpoints(file, vec![line]).await?;
                    }
                }
            }

            session.launch(program, cwd).await?;
            session.config_done().await?;

            registry.insert(session).await;

            if serve {
                let static_dir = static_dir.unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../crates/debugium-ui/dist")
                });
                server::start(hub, registry, port, static_dir, open_browser).await?;
            } else {
                // Stay alive waiting for events (interactive later)
                tokio::signal::ctrl_c().await?;
            }
        }

        Commands::Attach { port: _port, serve_port, serve, open_browser } => {
            let hub = Hub::new();
            let registry = SessionRegistry::new();

            if serve {
                let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../crates/debugium-ui/dist");
                server::start(hub, registry, serve_port, static_dir, open_browser).await?;
            }
        }
    }

    Ok(())
}
