#![deny(unsafe_code)]

mod color;
mod config;
mod constants;
mod gui;
mod hotkeys;
mod preview;
mod types;
mod x11_utils;

use anyhow::Result;
use clap::Parser;
use tracing::Level as TraceLevel;
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "eve-preview-manager")]
#[command(about = "EVE Online window preview manager", long_about = None)]
struct Cli {
    /// Run in preview daemon mode (background process showing thumbnails)
    #[arg(long)]
    preview: bool,
}

fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(TraceLevel::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set tracing subscriber");

    let cli = Cli::parse();

    if cli.preview {
        // Run preview daemon (background process showing thumbnails)
        preview::run_preview_daemon()
    } else {
        // Run GUI manager (default - manages preview process)
        gui::run_gui()
    }
}
