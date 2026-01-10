#![deny(unsafe_code)]

mod common;
mod config;
mod daemon;
mod input;
mod manager;
mod x11;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[command(name = "eve-preview-manager")]
#[command(version)]
#[command(about = "EVE Online window preview manager", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Enable debug mode with verbose logging and system diagnostics
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Internal: Run in daemon mode (background process)
    #[command(hide = true)]
    Daemon {
        /// Name of the IPC server to connect to for configuration and status updates
        #[arg(long)]
        ipc_server: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter_directives = if cli.debug {
        // Debug mode: detailed logs for our app, but keep noisy libraries (x11rb) at info
        "info,eve_preview_manager=debug"
    } else {
        "info"
    };

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter_directives));

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    match cli.command {
        Some(Commands::Daemon { ipc_server }) => {
            // Start the dedicated daemon process to isolate X11 rendering and overlay management
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
            // Default mode: launch the configuration Manager which manages the daemon lifecycle
            if cli.debug {
                crate::common::debug::log_system_info();
            }
            manager::run_manager(cli.debug)
        }
    }
}
