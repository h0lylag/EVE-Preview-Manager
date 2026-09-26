use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};

use crate::common::types::{Dimensions, Position, SourceIdentity};
use crate::config::DaemonConfig;

/// Spatial state for one thumbnail, shared by Manager/Daemon batch updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ThumbnailSpatialUpdate {
    pub source: SourceIdentity,
    pub position: Position,
    pub dimensions: Dimensions,
}

impl ThumbnailSpatialUpdate {
    pub fn new(source: SourceIdentity, position: Position, dimensions: Dimensions) -> Self {
        Self {
            source,
            position,
            dimensions,
        }
    }
}

/// Messages sent from Manager to Daemon
#[derive(Debug, Serialize, Deserialize)]
pub enum ConfigMessage {
    /// Complete configuration snapshot used only during daemon startup.
    ///
    /// Config-derived runtime resources such as cycle state and hotkey listeners are
    /// constructed from this payload. Applying later configuration changes requires
    /// restarting the daemon and sending a new startup snapshot.
    InitialConfig(Box<DaemonConfig>),

    /// Lightweight spatial deltas for one or more thumbnails.
    ///
    /// Used to acknowledge daemon-originated position batches without serializing the full
    /// configuration. The Daemon applies each update idempotently.
    ThumbnailMoves {
        updates: Vec<ThumbnailSpatialUpdate>,
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
    /// The Manager applies the complete batch to its active profile. When position auto-save
    /// is enabled, it persists the batch and acknowledges it with `ThumbnailMoves` without
    /// restarting the daemon.
    PositionsChanged {
        updates: Vec<ThumbnailSpatialUpdate>,
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
/// Contains the Manager-to-Daemon command sender and Daemon-to-Manager status receiver.
pub type BootstrapMessage = (IpcSender<ConfigMessage>, IpcReceiver<DaemonMessage>);

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ipc_channel::ipc::{self, IpcOneShotServer};

    use super::*;
    use crate::config::HotkeyBinding;
    use crate::config::profile::{CycleSlot, Profile};

    #[test]
    fn initial_config_round_trips_over_ipc() {
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
            thumbnail_opacity: 73,
            ..Profile::default()
        };
        let cycle_group = profile
            .cycle_groups
            .first_mut()
            .expect("default profile should contain a cycle group");
        cycle_group
            .cycle_list
            .push(CycleSlot::Eve("Test Character".to_string()));
        cycle_group.hotkey_forward = Some(binding.clone());
        profile
            .character_hotkeys
            .insert("Test Character".to_string(), binding.clone());

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
            .send(ConfigMessage::InitialConfig(Box::new(config)))
            .expect("initial config should serialize and send");

        let ConfigMessage::InitialConfig(received) =
            receiver.recv().expect("initial config should be received")
        else {
            panic!("expected an initial config message");
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
            received
                .profile
                .cycle_groups
                .first()
                .and_then(|group| group.hotkey_forward.as_ref()),
            Some(&binding)
        );
        assert_eq!(
            received.profile.character_hotkeys.get("Test Character"),
            Some(&binding)
        );
        assert_eq!(
            received.profile_hotkeys.get(&binding),
            Some(&received.profile.profile_name)
        );
        assert_eq!(received.profile.thumbnail_opacity, 73);
    }

    #[test]
    fn spatial_update_batches_round_trip_over_ipc() {
        let updates = vec![
            ThumbnailSpatialUpdate::new(
                SourceIdentity::eve("EVE Character"),
                Position::new(10, 20),
                Dimensions::new(300, 200),
            ),
            ThumbnailSpatialUpdate::new(
                SourceIdentity::custom("Custom Source"),
                Position::new(-50, 125),
                Dimensions::new(640, 360),
            ),
        ];
        let (config_sender, config_receiver) =
            ipc::channel::<ConfigMessage>().expect("config IPC channel should be created");
        let (status_sender, status_receiver) =
            ipc::channel::<DaemonMessage>().expect("status IPC channel should be created");

        config_sender
            .send(ConfigMessage::ThumbnailMoves {
                updates: updates.clone(),
            })
            .expect("thumbnail move batch should be sent");
        let ConfigMessage::ThumbnailMoves {
            updates: received_config,
        } = config_receiver
            .recv()
            .expect("thumbnail move batch should be received")
        else {
            panic!("expected thumbnail move batch");
        };
        assert_eq!(received_config, updates);

        status_sender
            .send(DaemonMessage::PositionsChanged {
                updates: updates.clone(),
            })
            .expect("positions changed batch should be sent");
        let DaemonMessage::PositionsChanged {
            updates: received_status,
        } = status_receiver
            .recv()
            .expect("positions changed batch should be received")
        else {
            panic!("expected positions changed batch");
        };
        assert_eq!(received_status, updates);
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
