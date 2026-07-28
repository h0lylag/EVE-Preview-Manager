use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};

use crate::config::DaemonConfig;

/// Messages sent from Manager to Daemon
#[derive(Debug, Serialize, Deserialize)]
pub enum ConfigMessage {
    /// Full state synchronization.
    ///
    /// Used for low-frequency, heavy operations like initial startup, profile switching,
    /// or bulk GUI setting changes. The payload is Boxed to reduce the enum size,
    /// optimizing the memory footprint for the high-frequency `ThumbnailMove` variant.
    Full(Box<DaemonConfig>),

    /// Lightweight spatial delta for a single thumbnail.
    ///
    /// Used during high-frequency drag events to avoid the overhead of full state serialization.
    /// The Daemon applies this incrementally and enforces idempotency to prevent redundant
    /// X11 re-configurations during rapid movement.
    ThumbnailMove {
        name: String,
        is_custom: bool,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    },

    /// Request a graceful daemon shutdown.
    ///
    /// The daemon should return from its event loop normally so X11 resources
    /// are released through existing Drop implementations.
    Shutdown,
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
    /// Notification that a thumbnail's spatial state was detected or changed by the Daemon.
    ///
    /// Upon receipt, the Manager updates its local state, saves to disk, and acknowledges
    /// with a `ThumbnailMove` delta. This confirms the new position without triggering
    /// a full config sync cycle.
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ipc_channel::ipc::{self, IpcOneShotServer};

    use super::*;
    use crate::config::HotkeyBinding;
    use crate::config::profile::{CycleSlot, Profile};

    #[test]
    fn full_config_round_trips_over_ipc() {
        let binding = HotkeyBinding::with_devices(
            15,
            true,
            false,
            false,
            false,
            vec!["keyboard".to_string()],
        );
        let mut profile = Profile {
            hotkey_toggle_previews: Some(binding.clone()),
            ..Profile::default()
        };
        profile
            .cycle_groups
            .first_mut()
            .expect("default profile should contain a cycle group")
            .cycle_list
            .push(CycleSlot::Eve("Test Character".to_string()));

        let mut profile_hotkeys = HashMap::new();
        profile_hotkeys.insert(binding.clone(), profile.profile_name.clone());

        let config = DaemonConfig {
            profile,
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys,
            runtime_hidden: false,
        };
        let (sender, receiver) =
            ipc::channel::<ConfigMessage>().expect("config IPC channel should be created");

        sender
            .send(ConfigMessage::Full(Box::new(config)))
            .expect("full config should serialize and send");

        let ConfigMessage::Full(received) =
            receiver.recv().expect("full config should be received")
        else {
            panic!("expected a full config message");
        };
        assert_eq!(
            received.profile.hotkey_toggle_previews.as_ref(),
            Some(&binding)
        );
        assert!(matches!(
            received
                .profile
                .cycle_groups
                .first()
                .and_then(|group| group.cycle_list.first()),
            Some(CycleSlot::Eve(name)) if name == "Test Character"
        ));
        assert_eq!(
            received.profile_hotkeys.get(&binding),
            Some(&received.profile.profile_name)
        );
    }

    #[test]
    fn bootstrap_channels_round_trip_over_one_shot_server() {
        let (server, server_name) = IpcOneShotServer::<BootstrapMessage>::new()
            .expect("one-shot IPC server should be created");
        let bootstrap_sender = IpcSender::connect(server_name)
            .expect("bootstrap sender should connect to the one-shot server");
        let (config_sender, config_receiver) =
            ipc::channel::<ConfigMessage>().expect("config IPC channel should be created");
        let (status_sender, status_receiver) =
            ipc::channel::<DaemonMessage>().expect("status IPC channel should be created");

        bootstrap_sender
            .send((config_sender, status_receiver))
            .expect("bootstrap channels should serialize and send");
        let (_, (accepted_config_sender, accepted_status_receiver)) = server
            .accept()
            .expect("bootstrap channels should be accepted");

        accepted_config_sender
            .send(ConfigMessage::Shutdown)
            .expect("accepted config sender should remain usable");
        assert!(matches!(
            config_receiver.recv(),
            Ok(ConfigMessage::Shutdown)
        ));

        status_sender
            .send(DaemonMessage::Heartbeat)
            .expect("status message should send through the transferred receiver");
        assert!(matches!(
            accepted_status_receiver.recv(),
            Ok(DaemonMessage::Heartbeat)
        ));
    }
}
