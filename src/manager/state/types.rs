use crate::common::constants::manager_ui::*;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ManagerTab {
    Behavior,
    Appearance,
    Hotkeys,
    Characters,
    Sources,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    Starting,
    Running(u32),
    Stopped,
    Crashed(Option<i32>),
}

impl DaemonStatus {
    pub fn color_rgb(&self) -> (u8, u8, u8) {
        match self {
            DaemonStatus::Running(_) => STATUS_RUNNING_RGB,
            DaemonStatus::Starting => STATUS_STARTING_RGB,
            _ => STATUS_STOPPED_RGB,
        }
    }

    pub fn label(&self) -> String {
        match self {
            DaemonStatus::Running(pid) => format!("Daemon running (PID: {})", pid),
            DaemonStatus::Starting => "Daemon starting...".to_string(),
            DaemonStatus::Stopped => "Daemon stopped".to_string(),
            DaemonStatus::Crashed(code) => match code {
                Some(code) => format!("Daemon crashed (exit {code})"),
                None => "Daemon crashed".to_string(),
            },
        }
    }
}

pub struct StatusMessage {
    pub text: String,
    pub color_rgb: (u8, u8, u8),
}
