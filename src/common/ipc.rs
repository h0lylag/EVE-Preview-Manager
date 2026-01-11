use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};

use crate::config::DaemonConfig;

/// Messages sent from Manager to Daemon
#[derive(Debug, Serialize, Deserialize)]
pub enum ConfigMessage {
    /// Update the entire daemon configuration
    Update(DaemonConfig),
}

/// Messages sent from Daemon to Manager
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonMessage {
    /// Log message from daemon
    Log {
        level: String,
        message: String,
    },
    /// New character window detected
    CharacterDetected {
        name: String,
        is_custom: bool,
    },
    /// Character thumbnail position changed (dragged)
    PositionChanged {
        name: String,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        is_custom: bool,
    },
    /// Daemon encountered an error
    Error(String),
    /// Generic status update for the Manager UI
    Status(String),
    RequestProfileSwitch(String),
    /// Periodic heartbeat (optional)
    Heartbeat,
}

/// The bootstrap payload sent over the initial server channel.
/// Contains the channel for receiving config updates and the channel for sending status updates.
pub type BootstrapMessage = (IpcSender<ConfigMessage>, IpcReceiver<DaemonMessage>);
