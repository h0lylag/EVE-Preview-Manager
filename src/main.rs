#![deny(unsafe_code)]

mod common;
mod config;
mod daemon;
mod input;
mod manager;
mod x11;

use anyhow::Result;
use clap::Parser;
use tracing::Level as TraceLevel;
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "eve-preview-manager")]
#[command(version)]
#[command(about = "EVE Online window preview manager", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Internal: Run in preview daemon mode (background process)
    #[command(hide = true)]
    Daemon {
        /// Name of the IPC server to connect to for configuration and status updates
        #[arg(long)]
        ipc_server: String,
    },
}

fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(TraceLevel::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Daemon { ipc_server }) => {
            // Start the dedicated preview process to isolate X11 rendering and overlay management
            // Initialize Tokio runtime for the daemon
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime");

            rt.block_on(async {
                if let Err(e) = daemon::run_daemon(ipc_server).await {
                    eprintln!("Daemon error: {e}");
                }
            });
            Ok(())
        }
        None => {
            // Default mode: launch the configuration GUI which manages the daemon lifecycle
            manager::run_gui()
        }
    }
}
