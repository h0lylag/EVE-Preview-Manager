//! X11 event processing for the daemon process
//!
//! Dispatcher that routes X11 events to specialized handlers.

use anyhow::Result;
use std::collections::HashMap;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event::{
    self, ConfigureNotify, CreateNotify, DamageNotify, DestroyNotify, PropertyNotify,
};

use super::cycle_state::CycleState;
use super::session_state::SessionState;
use super::thumbnail::Thumbnail;
use crate::config::DaemonConfig;

use crate::common::ipc::DaemonMessage;
use crate::x11::AppContext;
use ipc_channel::ipc::IpcSender;

use super::handlers;

/// Context bundle for event handlers to reduce argument count
pub struct EventContext<'a, 'b> {
    pub app_ctx: &'b AppContext<'a>,
    pub daemon_config: &'b mut DaemonConfig,
    pub eve_clients: &'b mut HashMap<Window, Thumbnail<'a>>,
    pub session_state: &'b mut SessionState,
    pub cycle_state: &'b mut CycleState,
    pub status_tx: &'b IpcSender<DaemonMessage>,
    pub font_renderer: &'b crate::daemon::font::FontRenderer,
    pub display_config: &'b crate::config::DisplayConfig,
}

pub fn handle_event(ctx: &mut EventContext, event: Event) -> Result<()> {
    match event {
        DamageNotify(event) => handlers::window::handle_damage_notify(ctx, event),
        CreateNotify(event) => handlers::window::handle_create_notify(ctx, event),
        Event::MapNotify(event) => handlers::window::handle_map_notify(ctx, event),
        DestroyNotify(event) => handlers::window::handle_destroy_notify(ctx, event),
        ConfigureNotify(event) => handlers::window::handle_configure_notify(ctx, event),
        Event::FocusIn(event) => handlers::state::handle_focus_in(ctx, event),
        Event::FocusOut(event) => handlers::state::handle_focus_out(ctx, event),
        Event::ButtonPress(event) => handlers::input::handle_button_press(ctx, event),
        Event::ButtonRelease(event) => handlers::input::handle_button_release(ctx, event),
        Event::MotionNotify(event) => handlers::input::handle_motion_notify(ctx, event),
        PropertyNotify(event) => {
            if event.atom == ctx.app_ctx.atoms.wm_name || event.atom == ctx.app_ctx.atoms.wm_class {
                handlers::window::handle_identity_update(ctx, event.window)
            } else if event.atom == ctx.app_ctx.atoms.net_wm_state {
                handlers::state::handle_net_wm_state(ctx, event.window, event.atom)
            } else {
                Ok(())
            }
        }
        Event::ReparentNotify(event) => {
            if let Some(thumbnail) = ctx.eve_clients.get_mut(&event.window) {
                thumbnail.set_parent(Some(event.parent));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
