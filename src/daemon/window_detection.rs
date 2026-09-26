//! EVE window detection and thumbnail creation logic

use anyhow::{Context, Result};
use ipc_channel::ipc::IpcSender;
use tracing::debug;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::ErrorKind;
use x11rb::protocol::xproto::*;

use crate::common::constants;
use crate::common::ipc::{DaemonMessage, ThumbnailSpatialUpdate};
use crate::common::types::{Dimensions, Position, SourceIdentity, SourceKind};
use crate::config::DaemonConfig;
use crate::config::DisplayConfig;
use crate::config::profile::CustomWindowRule;
use crate::x11::{AppContext, get_window_class, is_window_eve, is_window_minimized};
use std::collections::HashMap;

use super::session_state::SessionState;
use super::thumbnail::Thumbnail;

fn source_window_position(ctx: &AppContext, window: Window) -> Option<Position> {
    ctx.conn
        .get_geometry(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|geom| Position::new(geom.x, geom.y))
}

/// Identity of a detected EVE client or configured custom source.
#[derive(Debug, Clone)]
pub struct WindowIdentity {
    pub name: String,
    pub kind: SourceKind,
    pub rule: Option<CustomWindowRule>,
}

impl WindowIdentity {
    pub fn new_eve(name: String) -> Self {
        Self {
            name,
            kind: SourceKind::Eve,
            rule: None,
        }
    }

    pub fn new_custom(name: String, rule: CustomWindowRule) -> Self {
        Self {
            name,
            kind: SourceKind::Custom,
            rule: Some(rule),
        }
    }

    pub fn source_identity(&self) -> SourceIdentity {
        SourceIdentity::new(self.kind, self.name.clone())
    }

    pub fn is_eve(&self) -> bool {
        self.kind.is_eve()
    }

    pub fn is_custom(&self) -> bool {
        self.kind.is_custom()
    }
}

/// Exclude daemon-owned windows and previews from any EPM instance as sources.
/// Check before initial subscriptions and again after matching, before tracking.
fn should_ignore_source_window(ctx: &AppContext, window: Window) -> Result<bool> {
    let prop = match ctx
        .conn
        .get_property(
            false,
            window,
            ctx.atoms.net_wm_pid,
            AtomEnum::CARDINAL,
            0,
            1,
        )
        .context(format!("Failed to query _NET_WM_PID for {}", window))?
        .reply()
    {
        Ok(prop) => prop,
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => {
            return Ok(true); // The candidate disappeared before it could be inspected.
        }
        Err(error) => {
            return Err(error).context(format!("Failed to read _NET_WM_PID for {}", window));
        }
    };

    // EWMH defines a PID as one CARDINAL/32. Missing or malformed properties
    // provide no ownership information; still check the thumbnail class below.
    if prop.type_ == u32::from(AtomEnum::CARDINAL)
        && prop.bytes_after == 0
        && prop.value.len() == constants::x11::PID_PROPERTY_SIZE
        && prop.value32().and_then(|mut values| values.next()) == Some(std::process::id())
    {
        return Ok(true);
    }

    Ok(get_window_class(ctx.conn, window, ctx.atoms)?
        .is_some_and(|class| class.eq_ignore_ascii_case("eve-preview-thumbnail")))
}

/// Identify a window as either an EVE client or a Custom Source
pub fn identify_window(
    ctx: &AppContext,
    window: Window,
    state: &mut SessionState,
    custom_rules: &[CustomWindowRule],
) -> Result<Option<WindowIdentity>> {
    if should_ignore_source_window(ctx, window)? {
        return Ok(None);
    }

    // Add identity notifications without dropping this connection's existing
    // subscriptions. A tracked custom source may take the refresh path without
    // reinstalling its focus/structure mask during thumbnail creation.
    let attributes = match ctx
        .conn
        .get_window_attributes(window)
        .context(format!("Failed to query event mask for {}", window))?
        .reply()
    {
        Ok(attributes) => attributes,
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => {
            return Ok(None);
        }
        Err(error) => {
            return Err(error).context(format!("Failed to read event mask for {}", window));
        }
    };
    // InputOnly windows (including evdev's timestamp helper) have no drawable
    // content and must never become preview sources, even under broad rules.
    // X11 CreateWindow: https://www.x.org/releases/X11R7.7/doc/xproto/x11protocol.html
    if attributes.class == WindowClass::INPUT_ONLY {
        return Ok(None);
    }
    let event_mask = attributes.your_event_mask;
    if !event_mask.contains(EventMask::PROPERTY_CHANGE) {
        ctx.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(event_mask | EventMask::PROPERTY_CHANGE),
        )?;
    }

    if let Some(eve_window) = is_window_eve(ctx.conn, window, ctx.atoms)? {
        // A different client can finish setting ownership metadata while we read
        // the title. Recheck before updating session state or returning an identity.
        if should_ignore_source_window(ctx, window)? {
            return Ok(None);
        }

        let character_name = eve_window.character_name().to_string();
        debug!(window, character = %character_name, "Confirmed EVE Client");
        state.update_last_character(window, &character_name);

        ctx.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE | EventMask::STRUCTURE_NOTIFY,
            ),
        )?;

        return Ok(Some(WindowIdentity::new_eve(character_name)));
    }

    // Read title and class for custom-rule matching.
    let wm_name_cookie =
        ctx.conn
            .get_property(false, window, ctx.atoms.wm_name, AtomEnum::STRING, 0, 1024)?;

    let wm_class = get_window_class(ctx.conn, window, ctx.atoms)
        .ok()
        .flatten()
        .unwrap_or_default();

    // Get WM_NAME (Legacy)
    let wm_name_legacy = if let Ok(reply) = wm_name_cookie.reply() {
        String::from_utf8_lossy(&reply.value).to_string()
    } else {
        String::new()
    };

    // NOTE: Robust Title Fetching Strategy
    // Steam/Proton games often set title properties inconsistently or use non-UTF8 encodings.
    // To ensure reliable detection (especially at startup), we must check the full fallback chain:
    // 1. WM_NAME (Legacy X11)
    // 2. _NET_WM_NAME (Modern EWMH)
    // 3. _NET_WM_VISIBLE_NAME (Fallback for some compositors/toolkits)
    //
    // SAFETY: We use AtomEnum::ANY to accept any property type (UTF8_STRING, STRING, COMPOUND_TEXT).
    // Restricting to UTF8_STRING caused false negatives for valid windows.
    let wm_name = if !wm_name_legacy.is_empty() {
        wm_name_legacy.clone()
    } else {
        // Try _NET_WM_NAME (Any Type)
        let net_name = if let Ok(cookie) = ctx.conn.get_property(
            false,
            window,
            ctx.atoms.net_wm_name,
            AtomEnum::ANY, // Accept any type (UTF8_STRING, STRING, COMPOUND_TEXT)
            0,
            1024,
        ) {
            cookie
                .reply()
                .ok()
                .map(|r| String::from_utf8_lossy(&r.value).to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        if !net_name.is_empty() {
            net_name
        } else {
            // Try _NET_WM_VISIBLE_NAME (Any Type)
            if let Ok(cookie) = ctx.conn.get_property(
                false,
                window,
                ctx.atoms.net_wm_visible_name,
                AtomEnum::ANY,
                0,
                1024,
            ) {
                cookie
                    .reply()
                    .ok()
                    .map(|r| String::from_utf8_lossy(&r.value).to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
    };

    for rule in custom_rules {
        // Validation: If a pattern (title/class) is defined in the rule,
        // it acts as a strict filter that MUST match the window.
        let matches_title = rule
            .title_pattern
            .as_ref()
            .map(|p| wm_name.to_lowercase().contains(&p.to_lowercase()))
            .unwrap_or(false);

        let matches_class = rule
            .class_pattern
            .as_ref()
            .map(|p| wm_class.to_lowercase().contains(&p.to_lowercase()))
            .unwrap_or(false); // If rule has class pattern, it MUST match

        // Logic: Rule matches if...
        // - Title defined AND matches (AND Class is None OR matches)
        // - Class defined AND matches (AND Title is None OR matches)
        // Essentially, whatever criteria are defined must be satisfied.

        let mut matched = true;

        if rule.title_pattern.is_some() && !matches_title {
            matched = false;
        }
        if rule.class_pattern.is_some() && !matches_class {
            matched = false;
        }
        // If neither is defined, it's a catch-all? No, Manager enforces at least one.
        if rule.title_pattern.is_none() && rule.class_pattern.is_none() {
            matched = false;
        }

        if matched {
            // EPM publishes PID/class before its title. Another instance may have
            // completed that setup since the initial exclusion check.
            if should_ignore_source_window(ctx, window)? {
                return Ok(None);
            }

            debug!(
                window = window,
                alias = %rule.alias,
                title = %wm_name,
                class = %wm_class,
                "Identified Custom Source"
            );
            return Ok(Some(WindowIdentity::new_custom(
                rule.alias.clone(),
                rule.clone(),
            )));
        }
    }

    Ok(None)
}

#[allow(clippy::too_many_arguments)]
pub fn check_and_create_window<'a>(
    ctx: &AppContext<'a>,
    daemon_config: &DaemonConfig,
    display_config: &DisplayConfig,
    window: Window,
    font_renderer: &crate::daemon::font::FontRenderer,
    state: &mut SessionState,
    existing_thumbnails: &HashMap<Window, Thumbnail>,
    known_identity: Option<WindowIdentity>,
) -> Result<Option<Thumbnail<'a>>> {
    // Check if window matches EVE or Custom Rule
    let identity = if let Some(id) = known_identity {
        id
    } else {
        match identify_window(ctx, window, state, &daemon_config.profile.custom_windows)? {
            Some(id) => id,
            None => return Ok(None),
        }
    };

    // Apply Limit Logic for Custom Sources
    if identity.is_custom() {
        // FILTER 1: Must be mapped and viewable OR minimized
        // We removed the strict MapState::VIEWABLE check to allow minimized windows to be detected.
        // Utility windows are still filtered by `is_normal_window` below.

        if !crate::x11::is_normal_window(ctx.conn, window, ctx.atoms).unwrap_or(true) {
            debug!(window = window, alias = %identity.name, "Skipping non-normal custom source (utility/dock)");
            return Ok(None);
        }

        // IMPORTANT: Register for events on this custom source window!
        // identify_window does this for EVE clients; custom sources need it here.
        // We need:
        // - FOCUS_CHANGE: To detect when it gains/loses focus (for borders)
        // - PROPERTY_CHANGE: To detect name/state changes
        // - STRUCTURE_NOTIFY: To detect destruction/unmapping
        ctx.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE | EventMask::STRUCTURE_NOTIFY,
            ),
        )?;

        // Gather info for filtering and logging
        let mut width = 0;
        let mut height = 0;
        if let Ok(cookie) = ctx.conn.get_geometry(window)
            && let Ok(geom) = cookie.reply()
        {
            width = geom.width;
            height = geom.height;
        }

        let mut title = String::new();
        // Try WM_NAME (Legacy) first
        if let Ok(cookie) =
            ctx.conn
                .get_property(false, window, ctx.atoms.wm_name, AtomEnum::STRING, 0, 1024)
            && let Ok(reply) = cookie.reply()
        {
            title = String::from_utf8_lossy(&reply.value).to_string();
        }

        // Fallback 1: _NET_WM_NAME (Any Type)
        if title.is_empty()
            && let Some(reply) = ctx
                .conn
                .get_property(
                    false,
                    window,
                    ctx.atoms.net_wm_name,
                    AtomEnum::ANY, // Accept any type
                    0,
                    1024,
                )
                .ok()
                .and_then(|c| c.reply().ok())
        {
            let val = String::from_utf8_lossy(&reply.value).to_string();
            if !val.is_empty() {
                title = val;
            }
        }

        // Fallback 2: _NET_WM_VISIBLE_NAME (Any Type)
        if title.is_empty()
            && let Some(reply) = ctx
                .conn
                .get_property(
                    false,
                    window,
                    ctx.atoms.net_wm_visible_name,
                    AtomEnum::ANY, // Accept any type
                    0,
                    1024,
                )
                .ok()
                .and_then(|c| c.reply().ok())
        {
            title = String::from_utf8_lossy(&reply.value).to_string();
        }

        debug!(
            window = window,
            alias = %identity.name,
            width = width,
            height = height,
            title = %title,
            "Inspecting custom source candidate checks"
        );
    }

    if identity.rule.as_ref().is_some_and(|r| r.limit) {
        // Check if any EXISTING thumbnail has the same name
        // Note: existing_thumbnails contains previously processed windows
        if existing_thumbnails
            .values()
            .any(|t| t.source_kind().is_custom() && t.character_name == identity.name)
        {
            debug!(
                window = window,
                alias = %identity.name,
                "Skipping duplicate custom source (limit enabled)"
            );
            return Ok(None);
        }
    }

    // Cycle state registration is handled by scan_eve_windows at startup and
    // process_detected_window for Create, Map, and identity-change events.
    // This function is strictly for determining if we should create a renderable thumbnail.

    let remembered_character_name = if identity.is_eve() {
        state.window_last_character.get(&window).cloned()
    } else {
        None
    };
    let character_name = identity.name.clone();
    let effective_character_name = if character_name.is_empty() {
        remembered_character_name.as_deref().unwrap_or("")
    } else {
        character_name.as_str()
    };

    // Get saved position and dimensions
    // Determine which map to query based on identity type
    let settings_map = if identity.is_eve() {
        &daemon_config.character_thumbnails
    } else {
        &daemon_config.custom_source_thumbnails
    };

    let profile_map = if identity.is_eve() {
        &daemon_config.profile.character_thumbnails
    } else {
        &daemon_config.profile.custom_source_thumbnails
    };

    let runtime_settings = settings_map.get(effective_character_name);
    let profile_settings = profile_map.get(effective_character_name);
    let session_position = if runtime_settings.is_none() && profile_settings.is_none() {
        state.get_position(
            &character_name,
            window,
            &HashMap::new(),
            daemon_config.profile.thumbnail_preserve_position_on_swap,
        )
    } else {
        None
    };
    let position = daemon_config.resolve_initial_thumbnail_position(
        runtime_settings,
        profile_settings,
        session_position,
        source_window_position(ctx, window),
    );

    // Custom-source render overrides come from the rule when build_display_config()
    // merges it with saved per-source settings.
    let force_enable = display_config
        .settings_for(identity.kind, effective_character_name)
        .and_then(|s| s.override_render_preview)
        .unwrap_or(false);

    if !display_config.enabled && !force_enable {
        return Ok(None);
    }

    // Determine effective settings for dimensions and mode
    let effective_settings = settings_map
        .get(effective_character_name)
        .or_else(|| profile_map.get(effective_character_name));

    // Get dimensions: From settings, OR from Rule (if custom), OR default
    let (dimensions, preview_mode) = if let Some(settings) = effective_settings {
        // Use saved settings, but let Custom Rule override dimensions if present
        let dims = if let Some(rule) = &identity.rule {
            Dimensions::new(rule.default_width, rule.default_height)
        } else if settings.dimensions.width == 0 || settings.dimensions.height == 0 {
            // Auto-detect EVE default if saved dims are invalid
            let (w, h) = daemon_config
                .default_thumbnail_size(ctx.screen.width_in_pixels, ctx.screen.height_in_pixels);
            Dimensions::new(w, h)
        } else {
            settings.dimensions
        };
        // Use rule preview_mode if set, otherwise fallback to saved setting
        let mode = if let Some(rule) = &identity.rule
            && let Some(rule_mode) = &rule.preview_mode
        {
            rule_mode.clone()
        } else {
            settings.preview_mode.clone()
        };
        (dims, mode)
    } else {
        // No saved settings
        if let Some(rule) = &identity.rule {
            // Use Custom Rule defaults
            (
                Dimensions::new(rule.default_width, rule.default_height),
                rule.preview_mode.clone().unwrap_or_default(),
            )
        } else {
            // Auto-detect EVE default
            let (w, h) = daemon_config
                .default_thumbnail_size(ctx.screen.width_in_pixels, ctx.screen.height_in_pixels);
            (
                Dimensions::new(w, h),
                crate::common::types::PreviewMode::default(),
            )
        }
    };

    let mut thumbnail = Thumbnail::new(
        ctx,
        identity.kind,
        character_name.clone(),
        remembered_character_name,
        window,
        display_config,
        font_renderer,
        position,
        dimensions,
        preview_mode,
        daemon_config.runtime_hidden || (display_config.hide_when_no_focus && state.focus_hidden),
    )
    .context(format!(
        "Failed to create thumbnail for '{}' (window {})",
        character_name, window
    ))?;

    // Check minimized state
    let is_minimized = is_window_minimized(ctx.conn, window, ctx.atoms).unwrap_or(false);

    if is_minimized {
        thumbnail.minimized(display_config, font_renderer)?;
    } else {
        // NOTE: We rely on standard X11 Damage events to trigger the first update naturally.
        // Forcing an update here caused issues with fleeting windows.
    }

    debug!(
        window = window,
        character = %character_name,
        is_custom = identity.is_custom(),
        "Created thumbnail"
    );
    Ok(Some(thumbnail))
}

// Initial scan for existing EVE clients and custom sources to populate thumbnails.
use super::cycle_state::CycleState;

pub fn scan_eve_windows<'a>(
    ctx: &AppContext<'a>,
    display_config: &DisplayConfig,
    font_renderer: &crate::daemon::font::FontRenderer,
    daemon_config: &mut DaemonConfig,
    state: &mut SessionState,
    cycle_state: &mut CycleState,
    status_tx: &IpcSender<DaemonMessage>,
) -> Result<HashMap<Window, Thumbnail<'a>>> {
    let mut eve_clients = HashMap::new();

    // NOTE: Use _NET_CLIENT_LIST (EWMH) rather than query_tree(root) to get application
    // window IDs. Under reparenting WMs (e.g. KWin), query_tree(root) returns WM frame
    // windows whose properties (WM_CLASS, WM_NAME) don't match app rules, causing custom
    // sources and EVE clients to go undetected on daemon startup.
    let windows = crate::x11::get_client_list(ctx.conn, ctx.screen, ctx.atoms)
        .context("Failed to get window list via _NET_CLIENT_LIST")?;

    for w in windows {
        // 1. Identify valid windows (EVE or Custom Source)
        // We use identify_window directly so we can track them even if no thumbnail is created
        let identity = match identify_window(ctx, w, state, &daemon_config.profile.custom_windows) {
            Ok(Some(id)) => id,
            Ok(None) => continue, // Not a relevant window
            Err(e) => {
                tracing::warn!("Failed to identify window {} during scan: {}", w, e);
                continue;
            }
        };

        // Register identified window with CycleState
        let cycle_identity = (!identity.name.is_empty()).then(|| identity.source_identity());
        cycle_state.add_window(cycle_identity, w);

        // 2. Try to create thumbnail
        match check_and_create_window(
            ctx,
            daemon_config,
            display_config,
            w,
            font_renderer,
            state,
            &eve_clients,
            Some(identity.clone()),
        ) {
            Ok(Some(eve)) => {
                // Save initial position and dimensions (important for first-time characters)
                // Query geometry to get actual position from X11
                // We handle geometry query errors safely too, just in case
                let geom_result = ctx
                    .conn
                    .get_geometry(eve.window())
                    .map_err(anyhow::Error::from)
                    .and_then(|cookie| cookie.reply().map_err(anyhow::Error::from));

                match geom_result {
                    Ok(geom) => {
                        // Update the typed runtime settings map (skip logged-out clients with empty name).
                        let effective_character_name = eve.effective_character_name().to_string();
                        if !effective_character_name.is_empty() {
                            let settings = crate::common::types::CharacterSettings::new(
                                geom.x,
                                geom.y,
                                eve.dimensions.width,
                                eve.dimensions.height,
                            );

                            if eve.source_kind().is_custom() {
                                // NOTE: specific check to preserve existing overrides (like preview_mode)
                                // if they were already loaded from the profile config key.
                                if let Some(existing) = daemon_config
                                    .custom_source_thumbnails
                                    .get_mut(&effective_character_name)
                                {
                                    existing.x = settings.x;
                                    existing.y = settings.y;
                                    existing.dimensions = settings.dimensions;
                                } else {
                                    daemon_config
                                        .custom_source_thumbnails
                                        .insert(effective_character_name.clone(), settings);
                                }
                            } else if let Some(existing) = daemon_config
                                .character_thumbnails
                                .get_mut(&effective_character_name)
                            {
                                existing.x = settings.x;
                                existing.y = settings.y;
                                existing.dimensions = settings.dimensions;
                            } else {
                                daemon_config
                                    .character_thumbnails
                                    .insert(effective_character_name, settings);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to query geometry for new thumbnail window {}: {}",
                            eve.window(),
                            e
                        );
                        // Continue anyway, we just won't update the saved position
                    }
                }

                eve_clients.insert(w, eve);
            }
            Ok(None) => {
                // NOTE: Even with rendering disabled, new EVE characters and custom sources
                // must reach the Manager via PositionsChanged so they appear for configuration.
                if !display_config.enabled && !identity.name.is_empty() {
                    let is_new = if identity.is_eve() {
                        !daemon_config
                            .character_thumbnails
                            .contains_key(&identity.name)
                            && !daemon_config
                                .profile
                                .character_thumbnails
                                .contains_key(&identity.name)
                    } else {
                        !daemon_config
                            .custom_source_thumbnails
                            .contains_key(&identity.name)
                            && !daemon_config
                                .profile
                                .custom_source_thumbnails
                                .contains_key(&identity.name)
                    };

                    if is_new {
                        let (ww, hh) = (
                            daemon_config.profile.thumbnail_default_width,
                            daemon_config.profile.thumbnail_default_height,
                        );
                        let spawn_position = daemon_config
                            .fallback_new_thumbnail_position(source_window_position(ctx, w))
                            .unwrap_or_default();

                        let settings = crate::common::types::CharacterSettings::new(
                            spawn_position.x,
                            spawn_position.y,
                            ww,
                            hh,
                        );
                        if identity.is_eve() {
                            daemon_config
                                .character_thumbnails
                                .insert(identity.name.clone(), settings);
                        } else {
                            daemon_config
                                .custom_source_thumbnails
                                .insert(identity.name.clone(), settings);
                        }
                        let update = ThumbnailSpatialUpdate::new(
                            identity.source_identity(),
                            spawn_position,
                            Dimensions::new(ww, hh),
                        );
                        let _ = status_tx.send(DaemonMessage::PositionsChanged {
                            updates: vec![update],
                        });
                        let _ = status_tx.send(DaemonMessage::CharacterDetected {
                            name: identity.name.clone(),
                            is_custom: identity.is_custom(),
                        });
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create thumbnail for window {} during scan: {}",
                    w,
                    e
                );
            }
        }
    }

    ctx.conn
        .flush()
        .context("Failed to flush X11 connection after creating thumbnails")?;
    Ok(eve_clients)
}

#[cfg(test)]
mod tests {
    //! Display-dependent source detection regressions. Run only on an isolated Xvfb display.
    //! The Fontdue text-alpha regression requires DejaVu Sans to be installed and
    //! visible to fontconfig; it fails rather than silently using the X11 fallback.
    //!
    //! From the repository root, enter `nix develop`, then run:
    //!
    //! ```sh
    //! cargo test --locked --all-features --no-run
    //! xvfb-run -a -s "-screen 0 1280x800x24 -nolisten tcp -noreset" \
    //!   env EPM_X11_TESTS=1 timeout 60s \
    //!   cargo test --locked --all-features daemon::window_detection::tests -- --ignored --test-threads=1
    //! ```

    use crate::daemon::{
        cycle_state::CycleState,
        dispatcher::{EventContext, handle_event},
        font::FontRenderer,
        group_drag::GroupDragState,
        session_state::SessionState,
    };
    use crate::{
        common::ipc::DaemonMessage,
        config::{DaemonConfig, profile::Profile},
        x11::{AppContext, CachedAtoms, CachedFormats},
    };
    use ipc_channel::{
        TryRecvError,
        ipc::{self, IpcReceiver},
    };
    use std::collections::HashMap;
    use x11rb::{
        connection::Connection,
        protocol::{Event, xproto::*},
        wrapper::ConnectionExt as _,
    };

    fn with_x11(test: impl FnOnce(&AppContext<'_>)) {
        assert_eq!(
            std::env::var("EPM_X11_TESTS").as_deref(),
            Ok("1"),
            "run display tests with EPM_X11_TESTS=1 under an isolated Xvfb server"
        );
        let (conn, screen_number) = x11rb::connect(None).expect("connect to isolated Xvfb");
        let screen = &conn.setup().roots[screen_number];
        let atoms = CachedAtoms::new(&conn).unwrap();
        let formats = CachedFormats::new(&conn, screen).unwrap();
        let ctx = AppContext {
            conn: &conn,
            screen,
            atoms: &atoms,
            formats: &formats,
        };
        // All fixture windows are owned by this connection and die when it closes.
        test(&ctx);
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn input_only_timestamp_windows_are_not_preview_sources() {
        with_x11(|ctx| {
            let helper = ctx.conn.generate_id().unwrap();
            ctx.conn
                .create_window(
                    0,
                    helper,
                    ctx.screen.root,
                    0,
                    0,
                    1,
                    1,
                    0,
                    WindowClass::INPUT_ONLY,
                    0,
                    &CreateWindowAux::new(),
                )
                .unwrap()
                .check()
                .unwrap();
            // Imported configurations can contain an empty substring pattern.
            let rules: Vec<super::CustomWindowRule> = serde_json::from_value(serde_json::json!([
                { "alias": "Broad match", "title_pattern": "", "limit": false }
            ]))
            .unwrap();
            assert!(
                super::identify_window(ctx, helper, &mut SessionState::new(), &rules)
                    .unwrap()
                    .is_none()
            );
            assert!(
                !ctx.conn
                    .get_window_attributes(helper)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .your_event_mask
                    .contains(EventMask::PROPERTY_CHANGE)
            );
        });
    }

    fn with_sources(
        ctx: &AppContext<'_>,
        test: impl FnOnce(&mut EventContext<'_, '_>, &IpcReceiver<DaemonMessage>),
    ) {
        let rule = serde_json::from_value(serde_json::json!({
            "alias": "YouTube", "title_pattern": "YouTube", "limit": false
        }))
        .unwrap();
        let config = DaemonConfig {
            profile: Profile {
                custom_windows: vec![rule],
                ..Profile::default()
            },
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        };
        with_config(ctx, config, test);
    }

    fn with_config<'a>(
        ctx: &AppContext<'a>,
        config: DaemonConfig,
        test: impl FnOnce(&mut EventContext<'a, '_>, &IpcReceiver<DaemonMessage>),
    ) {
        let font = FontRenderer::resolve_from_config(ctx.conn, "sans-serif", 12.0).unwrap();
        with_config_and_font(ctx, config, &font, test);
    }

    fn with_config_and_font<'a>(
        ctx: &AppContext<'a>,
        mut config: DaemonConfig,
        font: &FontRenderer,
        test: impl FnOnce(&mut EventContext<'a, '_>, &IpcReceiver<DaemonMessage>),
    ) {
        let display = config.build_display_config();
        let mut previews = HashMap::new();
        let mut session = SessionState::new();
        let mut cycle = CycleState::new(config.profile.cycle_groups.clone());
        let mut drag = GroupDragState::default();
        let (tx, rx) = ipc::channel().unwrap();
        test(
            &mut EventContext {
                app_ctx: ctx,
                daemon_config: &mut config,
                eve_clients: &mut previews,
                session_state: &mut session,
                cycle_state: &mut cycle,
                group_drag_state: &mut drag,
                status_tx: &tx,
                font_renderer: font,
                display_config: &display,
            },
            &rx,
        );
    }

    fn set_title(ctx: &AppContext<'_>, window: Window, title: &str) {
        ctx.conn
            .change_property8(
                PropMode::REPLACE,
                window,
                ctx.atoms.wm_name,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap()
            .check()
            .unwrap();
    }

    fn window(ctx: &AppContext<'_>, title: &str, class: &str) -> Window {
        let window = ctx.conn.generate_id().unwrap();
        ctx.conn
            .create_window(
                ctx.screen.root_depth,
                window,
                ctx.screen.root,
                0,
                0,
                500,
                400,
                0,
                WindowClass::INPUT_OUTPUT,
                ctx.screen.root_visual,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
        set_title(ctx, window, title);
        ctx.conn
            .change_property8(
                PropMode::REPLACE,
                window,
                ctx.atoms.wm_class,
                AtomEnum::STRING,
                format!("instance\0{class}\0").as_bytes(),
            )
            .unwrap()
            .check()
            .unwrap();
        ctx.conn.map_window(window).unwrap().check().unwrap();
        window
    }

    fn create_event(ctx: &AppContext<'_>, window: Window) -> Event {
        Event::CreateNotify(CreateNotifyEvent {
            response_type: CREATE_NOTIFY_EVENT,
            parent: ctx.screen.root,
            window,
            width: 500,
            height: 400,
            ..Default::default()
        })
    }

    fn detection_events(ctx: &AppContext<'_>, window: Window) -> [Event; 4] {
        [
            create_event(ctx, window),
            Event::MapNotify(MapNotifyEvent {
                response_type: MAP_NOTIFY_EVENT,
                event: ctx.screen.root,
                window,
                ..Default::default()
            }),
            property_event(window, ctx.atoms.wm_name),
            property_event(window, ctx.atoms.wm_class),
        ]
    }

    fn property_event(window: Window, atom: Atom) -> Event {
        Event::PropertyNotify(PropertyNotifyEvent {
            response_type: PROPERTY_NOTIFY_EVENT,
            window,
            atom,
            state: Property::NEW_VALUE,
            ..Default::default()
        })
    }

    fn drain_messages(rx: &IpcReceiver<DaemonMessage>) {
        // A bounded drain also catches an unexpected stream of source registrations.
        for _ in 0..32 {
            match rx.try_recv() {
                Ok(_) => {}
                Err(TryRecvError::Empty) => return,
                Err(error) => panic!("unexpected IPC error: {error}"),
            }
        }
        panic!("too many source registration messages");
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn preview_events_do_not_create_recursive_sources() {
        with_x11(|ctx| {
            with_sources(ctx, |events, rx| {
                let src = window(ctx, "YouTube", "browser");
                handle_event(events, create_event(ctx, src)).unwrap();
                assert_eq!(events.eve_clients.len(), 1);
                let preview = events.eve_clients[&src].window();
                let config_before = serde_json::to_value(&*events.daemon_config).unwrap();
                let sources_before = events.cycle_state.get_active_windows().clone();
                let positions_before = events.session_state.window_positions.clone();
                let characters_before = events.session_state.window_last_character.clone();
                drain_messages(rx);

                // Fail on the first extra preview, rather than allowing runaway allocation.
                for _ in 0..3 {
                    set_title(ctx, preview, "EPM Thumbnail - YouTube renamed");
                    for event in detection_events(ctx, preview) {
                        handle_event(events, event).unwrap();
                        assert_eq!(events.eve_clients.len(), 1, "preview became a source");
                    }
                }
                assert_eq!(*events.cycle_state.get_active_windows(), sources_before);
                assert_eq!(events.session_state.window_positions, positions_before);
                assert_eq!(
                    events.session_state.window_last_character,
                    characters_before
                );
                assert_eq!(
                    serde_json::to_value(&*events.daemon_config).unwrap(),
                    config_before
                );
                assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn custom_source_redetection_preserves_event_subscriptions() {
        with_x11(|ctx| {
            with_sources(ctx, |events, _rx| {
                let src = window(ctx, "YouTube", "browser");
                handle_event(events, create_event(ctx, src)).unwrap();
                let preview = events.eve_clients[&src].window();
                let required = EventMask::PROPERTY_CHANGE
                    | EventMask::FOCUS_CHANGE
                    | EventMask::STRUCTURE_NOTIFY;

                for _ in 0..3 {
                    for event in detection_events(ctx, src) {
                        handle_event(events, event).unwrap();
                        let mask = ctx
                            .conn
                            .get_window_attributes(src)
                            .unwrap()
                            .reply()
                            .unwrap()
                            .your_event_mask;
                        assert!(
                            mask.contains(required),
                            "custom source lost subscriptions: {mask:?}"
                        );
                        assert_eq!(events.eve_clients.len(), 1);
                        assert_eq!(events.eve_clients[&src].window(), preview);
                    }
                }

                // Check real server delivery, not only the reported event mask.
                ctx.conn
                    .set_input_focus(InputFocus::POINTER_ROOT, src, x11rb::CURRENT_TIME)
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn
                    .configure_window(src, &ConfigureWindowAux::new().width(520))
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn.get_input_focus().unwrap().reply().unwrap();
                let mut saw_focus = false;
                let mut saw_configure = false;
                for _ in 0..128 {
                    let Some(event) = ctx.conn.poll_for_event().unwrap() else {
                        break;
                    };
                    match event {
                        Event::FocusIn(event) if event.event == src => saw_focus = true,
                        Event::ConfigureNotify(event) if event.window == src => {
                            saw_configure = true
                        }
                        _ => {}
                    }
                }
                assert!(saw_focus, "source FocusIn was not delivered");
                assert!(saw_configure, "source ConfigureNotify was not delivered");
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn create_notifications_preserve_preview_mouse_subscriptions() {
        with_x11(|ctx| {
            with_sources(ctx, |events, _rx| {
                let src = window(ctx, "YouTube", "browser");
                handle_event(events, create_event(ctx, src)).unwrap();
                let preview = events.eve_clients[&src].window();
                let before = ctx
                    .conn
                    .get_window_attributes(preview)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .your_event_mask;
                assert!(before.contains(
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION
                ));
                // Isolate subscription preservation from whether a custom rule matches.
                events.daemon_config.profile.custom_windows.clear();
                for event in detection_events(ctx, preview) {
                    handle_event(events, event).unwrap();
                    let after = ctx
                        .conn
                        .get_window_attributes(preview)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .your_event_mask;
                    assert_eq!(
                        after, before,
                        "source discovery replaced preview input subscriptions"
                    );
                }
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn text_alpha_x11_composites_global_and_source_colors_correctly() {
        with_x11(|ctx| {
            let font_id = ctx.conn.generate_id().unwrap();
            ctx.conn
                .open_font(font_id, b"fixed")
                .unwrap()
                .check()
                .unwrap();
            let font = FontRenderer::X11Fallback {
                font_id,
                size: 12.0,
            };
            check_text_alpha(ctx, &font);
            ctx.conn.close_font(font_id).unwrap().check().unwrap();
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and DejaVu Sans; see test module for command"]
    fn text_alpha_fontdue_composites_global_and_source_colors_correctly() {
        let font = FontRenderer::from_font_name("DejaVu Sans", 12.0)
            .expect("text alpha regression requires DejaVu Sans installed for Fontdue");
        assert!(matches!(font, FontRenderer::Fontdue { .. }));
        with_x11(|ctx| check_text_alpha(ctx, &font));
    }

    fn check_text_alpha(ctx: &AppContext<'_>, font: &FontRenderer) {
        use crate::common::types::{CharacterSettings, PreviewMode};

        for custom in [false, true] {
            for per_source in [false, true] {
                let make_config = |color: Option<&str>| {
                    let mut profile = Profile {
                        thumbnail_opacity: 100,
                        thumbnail_active_border: false,
                        thumbnail_inactive_border: false,
                        thumbnail_text_x: 10,
                        thumbnail_text_y: 10,
                        thumbnail_text_color: if per_source {
                            "#FF00FF00"
                        } else {
                            color.unwrap()
                        }
                        .into(),
                        ..Profile::default()
                    };
                    let mut settings = CharacterSettings::new(20, 20, 160, 100);
                    settings.preview_mode = PreviewMode::Static {
                        color: "#0000FF".into(),
                    };
                    if custom {
                        profile.custom_windows.push(
                            serde_json::from_value(serde_json::json!({
                                "alias": "Review", "title_pattern": "Review source",
                                "default_width": 160, "default_height": 100,
                                "text_color": if per_source { color } else { None }
                            }))
                            .unwrap(),
                        );
                        profile
                            .custom_source_thumbnails
                            .insert("Review".into(), settings);
                    } else {
                        settings.override_text_color = if per_source {
                            color.map(str::to_owned)
                        } else {
                            None
                        };
                        profile
                            .character_thumbnails
                            .insert("Review".into(), settings);
                    }
                    // Exercise the persisted profile contract before runtime override resolution.
                    let json = serde_json::to_vec(&profile).unwrap();
                    let profile: Profile = serde_json::from_slice(&json).unwrap();
                    DaemonConfig {
                        character_thumbnails: profile.character_thumbnails.clone(),
                        custom_source_thumbnails: profile.custom_source_thumbnails.clone(),
                        profile,
                        profile_hotkeys: HashMap::new(),
                        runtime_hidden: false,
                    }
                };
                with_config_and_font(ctx, make_config(Some("#FFFF0000")), font, |events, _| {
                    let (title, class) = if custom {
                        ("Review source", "browser")
                    } else {
                        ("EVE - Review", "eve")
                    };
                    let src = window(ctx, title, class);
                    handle_event(events, create_event(ctx, src)).unwrap();
                    let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                    let read_pixels = |window| {
                        let reply = ctx
                            .conn
                            .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, 160, 100, u32::MAX)
                            .unwrap()
                            .reply()
                            .unwrap();
                        let pixels: Vec<u32> = reply
                            .data
                            .chunks_exact(4)
                            .map(|bytes| {
                                let bytes = bytes.try_into().unwrap();
                                if ctx.conn.setup().image_byte_order == ImageOrder::LSB_FIRST {
                                    u32::from_le_bytes(bytes)
                                } else {
                                    u32::from_be_bytes(bytes)
                                }
                            })
                            .collect();
                        assert_eq!(pixels.len(), 160 * 100);
                        pixels
                    };
                    let mut opaque_coverage = Vec::new();
                    for color in [
                        Some("#FFFF0000"),
                        Some("#00FF0000"),
                        Some("#80FF0000"),
                        Some("#FF0000"),
                        None,
                    ] {
                        if color.is_none() && !per_source {
                            continue;
                        }
                        let display = make_config(color).build_display_config();
                        thumbnail.border(&display, false, false, font).unwrap();
                        thumbnail.update(&display, font).unwrap();
                        let pixels = read_pixels(thumbnail.window());
                        if opaque_coverage.is_empty() {
                            opaque_coverage =
                                pixels.iter().map(|pixel| (pixel >> 16) & 255).collect();
                            assert!(
                                opaque_coverage.iter().any(|&coverage| coverage > 0),
                                "fixture must render visible glyphs"
                            );
                        }
                        for (index, (&pixel, &coverage)) in
                            pixels.iter().zip(&opaque_coverage).enumerate()
                        {
                            let alpha = match color {
                                Some("#00FF0000") => 0,
                                Some("#80FF0000") => 128,
                                _ => 255,
                            };
                            let contribution = coverage * alpha / 255;
                            let expected = if color.is_none() {
                                [0, contribution, 255 - contribution]
                            } else {
                                [contribution, 0, 255 - contribution]
                            };
                            let actual = [(pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255];
                            for (actual, expected) in actual.into_iter().zip(expected) {
                                assert!(
                                    actual.abs_diff(expected) <= 1,
                                    "custom={custom}, per_source={per_source}, color={color:?}, pixel={index}: channel {actual} != {expected}"
                                );
                            }
                        }
                    }
                    // Text must blend over the skip indicator without erasing it.
                    let transparent = make_config(Some("#00FF0000")).build_display_config();
                    thumbnail.border(&transparent, false, true, font).unwrap();
                    thumbnail.update(&transparent, font).unwrap();
                    let skipped = read_pixels(thumbnail.window());
                    let partial = make_config(Some("#80FF0000")).build_display_config();
                    thumbnail.border(&partial, false, true, font).unwrap();
                    thumbnail.update(&partial, font).unwrap();
                    let pixels = read_pixels(thumbnail.window());
                    assert!(
                        skipped.iter().zip(&opaque_coverage).any(|(&pixel, &coverage)|
                            pixel & 0xFFFFFF == 0xFF0000 && coverage > 0
                        ),
                        "fixture must overlap text and skip indicator",
                    );
                    for ((&pixel, &base), &coverage) in
                        pixels.iter().zip(&skipped).zip(&opaque_coverage)
                    {
                        let alpha = coverage * 128 / 255;
                        for (shift, foreground) in [(16, alpha), (8, 0), (0, 0)] {
                            let expected =
                                foreground + ((base >> shift) & 255) * (255 - alpha) / 255;
                            let actual = (pixel >> shift) & 255;
                            assert!(
                                actual.abs_diff(expected) <= 1,
                                "text erased the skip indicator: pixel={pixel:08X}, base={base:08X}, coverage={coverage}"
                            );
                        }
                    }
                    ctx.conn.destroy_window(src).unwrap().check().unwrap();
                });
            }
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn border_alpha_composites_global_and_source_colors_correctly() {
        use crate::common::types::{CharacterSettings, PreviewMode};
        for custom in [false, true] {
            for per_source in [false, true] {
                for (color, expected) in [
                    ("#00FF0000", 0x0000FF),
                    ("#80FF0000", 0x80007F),
                    ("#FFFF0000", 0xFF0000),
                ] {
                    with_x11(|ctx| {
                        let global_color = if per_source { "#FF00FF00" } else { color };
                        let mut profile = Profile {
                            thumbnail_active_border: true,
                            thumbnail_active_border_size: 5,
                            thumbnail_active_border_color: global_color.into(),
                            thumbnail_inactive_border: true,
                            thumbnail_inactive_border_size: 5,
                            thumbnail_inactive_border_color: global_color.into(),
                            thumbnail_text_color: "#00000000".into(),
                            ..Profile::default()
                        };
                        let mut settings = CharacterSettings::new(20, 20, 160, 100);
                        settings.preview_mode = PreviewMode::Static {
                            color: "#0000FF".into(),
                        };
                        let (title, class) = if custom {
                            profile.custom_windows.push(serde_json::from_value(serde_json::json!({
                                "alias": "Review", "title_pattern": "Review source", "default_width": 160, "default_height": 100,
                                "active_border_color": per_source.then_some(color),
                                "inactive_border_color": per_source.then_some(color)
                            })).unwrap());
                            profile
                                .custom_source_thumbnails
                                .insert("Review".into(), settings);
                            ("Review source", "browser")
                        } else {
                            settings.override_active_border_color =
                                per_source.then(|| color.into());
                            settings.override_inactive_border_color =
                                per_source.then(|| color.into());
                            profile
                                .character_thumbnails
                                .insert("Review".into(), settings);
                            ("EVE - Review", "eve")
                        };
                        let config = DaemonConfig {
                            character_thumbnails: profile.character_thumbnails.clone(),
                            custom_source_thumbnails: profile.custom_source_thumbnails.clone(),
                            profile,
                            profile_hotkeys: HashMap::new(),
                            runtime_hidden: false,
                        };
                        with_config(ctx, config, |events, _| {
                            let src = window(ctx, title, class);
                            handle_event(events, create_event(ctx, src)).unwrap();
                            let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                            for focused in [false, true] {
                                thumbnail
                                    .border(
                                        events.display_config,
                                        focused,
                                        false,
                                        events.font_renderer,
                                    )
                                    .unwrap();
                                thumbnail
                                    .update(events.display_config, events.font_renderer)
                                    .unwrap();
                                let read_pixel = |x, y| {
                                    let reply = ctx
                                        .conn
                                        .get_image(
                                            ImageFormat::Z_PIXMAP,
                                            thumbnail.window(),
                                            x,
                                            y,
                                            1,
                                            1,
                                            u32::MAX,
                                        )
                                        .unwrap()
                                        .reply()
                                        .unwrap();
                                    let bytes = reply.data[..4].try_into().unwrap();
                                    let pixel = if ctx.conn.setup().image_byte_order
                                        == ImageOrder::LSB_FIRST
                                    {
                                        u32::from_le_bytes(bytes)
                                    } else {
                                        u32::from_be_bytes(bytes)
                                    };
                                    pixel & 0x00FF_FFFF
                                };
                                assert_eq!(read_pixel(80, 70), 0x0000FF);
                                assert_eq!(
                                    read_pixel(1, 1),
                                    expected,
                                    "custom={custom}, per_source={per_source}, focused={focused}, color={color}"
                                );
                            }
                        });
                    });
                }
            }
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn static_preview_rejects_malformed_colors_and_recovers() {
        use crate::common::types::{CharacterSettings, PreviewMode};
        with_x11(|ctx| {
            with_sources(ctx, |events, _| {
                for (title, class, name, custom) in [
                    ("EVE - Alice", "eve", "Alice", false),
                    ("YouTube", "browser", "YouTube", true),
                ] {
                    let src = window(ctx, title, class);
                    handle_event(events, create_event(ctx, src)).unwrap();
                    let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                    let mut display = events.display_config.clone();
                    for color in [
                        "#€ABC",
                        "#€ABCDE",
                        "#😀12",
                        "#😀1234",
                        "##123456",
                        "1234567",
                    ] {
                        let settings = if custom {
                            &mut display.custom_source_settings
                        } else {
                            &mut display.character_settings
                        };
                        settings
                            .entry(name.into())
                            .or_insert_with(|| CharacterSettings::new(0, 0, 160, 100))
                            .preview_mode = PreviewMode::Static {
                            color: color.into(),
                        };
                        let error = thumbnail
                            .update(&display, events.font_renderer)
                            .unwrap_err();
                        assert!(error.to_string().contains("Invalid hex color"));
                        assert!(thumbnail.is_visible());
                        // A subsequent valid update must still work on the same renderer.
                        let settings = if custom {
                            &mut display.custom_source_settings
                        } else {
                            &mut display.character_settings
                        };
                        settings.get_mut(name).unwrap().preview_mode = PreviewMode::Static {
                            color: "#00000000".into(),
                        };
                        thumbnail.update(&display, events.font_renderer).unwrap();
                    }
                    ctx.conn.get_input_focus().unwrap().reply().unwrap();
                }
            });
        });
    }

    fn visibility_config() -> DaemonConfig {
        use crate::common::types::CharacterSettings;
        let mut profile = Profile {
            thumbnail_hide_not_focused: true,
            ..Profile::default()
        };
        profile.character_thumbnails.insert(
            "Alice".into(),
            CharacterSettings {
                override_render_preview: Some(false),
                ..CharacterSettings::new(10, 20, 160, 100)
            },
        );
        profile.character_thumbnails.insert(
            "Bob".into(),
            CharacterSettings {
                override_render_preview: Some(true),
                ..CharacterSettings::new(30, 40, 160, 100)
            },
        );
        profile.custom_windows.push(
            serde_json::from_value(serde_json::json!({
                "alias": "Alice", "title_pattern": "Custom Alice", "override_render_preview": true
            }))
            .unwrap(),
        );
        DaemonConfig {
            character_thumbnails: profile.character_thumbnails.clone(),
            custom_source_thumbnails: profile.custom_source_thumbnails.clone(),
            profile,
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        }
    }

    fn assert_visible(
        ctx: &AppContext<'_>,
        events: &EventContext<'_, '_>,
        src: Window,
        expected: bool,
    ) {
        let thumbnail = &events.eve_clients[&src];
        assert_eq!(thumbnail.is_visible(), expected);
        let map_state = ctx
            .conn
            .get_window_attributes(thumbnail.window())
            .unwrap()
            .reply()
            .unwrap()
            .map_state;
        assert_eq!(
            map_state,
            if expected {
                MapState::VIEWABLE
            } else {
                MapState::UNMAPPED
            }
        );
    }

    fn swap_character(
        ctx: &AppContext<'_>,
        events: &mut EventContext<'_, '_>,
        src: Window,
        name: &str,
    ) {
        set_title(ctx, src, &format!("EVE - {name}"));
        handle_event(events, property_event(src, ctx.atoms.wm_name)).unwrap();
    }

    fn focus_in(events: &mut EventContext<'_, '_>, src: Window) {
        handle_event(
            events,
            Event::FocusIn(FocusInEvent {
                response_type: FOCUS_IN_EVENT,
                event: src,
                mode: NotifyMode::NORMAL,
                detail: NotifyDetail::NONLINEAR,
                ..Default::default()
            }),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_character_swaps_respect_all_hiding_reasons() {
        use crate::daemon::handlers::state::{hide_after_focus_loss, toggle_previews};
        with_x11(|ctx| {
            for focus_hidden in [false, true] {
                for runtime_hidden in [false, true] {
                    let mut config = visibility_config();
                    config.runtime_hidden = runtime_hidden;
                    with_config(ctx, config, |events, _| {
                        if focus_hidden {
                            hide_after_focus_loss(events);
                        }
                        let src = window(ctx, "EVE - Alice", "eve");
                        handle_event(events, create_event(ctx, src)).unwrap();
                        assert_visible(ctx, events, src, false);
                        swap_character(ctx, events, src, "Bob");
                        assert_visible(ctx, events, src, !focus_hidden && !runtime_hidden);
                        // Logout retains Bob's remembered identity and render override.
                        set_title(ctx, src, "EVE");
                        handle_event(events, property_event(src, ctx.atoms.wm_name)).unwrap();
                        assert_eq!(events.eve_clients[&src].effective_character_name(), "Bob");
                        assert_visible(ctx, events, src, !focus_hidden && !runtime_hidden);
                        swap_character(ctx, events, src, "Alice");
                        assert_visible(ctx, events, src, false);
                        if runtime_hidden {
                            toggle_previews(events);
                        }
                        focus_in(events, src);
                        assert_visible(ctx, events, src, false);
                        swap_character(ctx, events, src, "Bob");
                        assert_visible(ctx, events, src, true);
                    });
                }
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_focus_return_does_not_undo_toggle() {
        use crate::daemon::handlers::state::{hide_after_focus_loss, toggle_previews};
        with_x11(|ctx| {
            with_config(ctx, visibility_config(), |events, _| {
                let src = window(ctx, "EVE - Bob", "eve");
                handle_event(events, create_event(ctx, src)).unwrap();
                focus_in(events, src);
                toggle_previews(events);
                assert_visible(ctx, events, src, false);
                handle_event(
                    events,
                    Event::FocusOut(FocusOutEvent {
                        response_type: FOCUS_OUT_EVENT,
                        event: src,
                        mode: NotifyMode::NORMAL,
                        detail: NotifyDetail::NONLINEAR,
                        ..Default::default()
                    }),
                )
                .unwrap();
                assert!(events.session_state.focus_loss_deadline.is_some());
                focus_in(events, src);
                assert!(events.session_state.focus_loss_deadline.is_none());
                assert!(events.daemon_config.runtime_hidden);
                assert_visible(ctx, events, src, false);
                hide_after_focus_loss(events);
                toggle_previews(events);
                assert!(!events.daemon_config.runtime_hidden);
                assert_visible(ctx, events, src, false);
                events
                    .eve_clients
                    .get_mut(&src)
                    .unwrap()
                    .minimized(events.display_config, events.font_renderer)
                    .unwrap();
                let other = window(ctx, "Custom Alice", "browser");
                handle_event(events, create_event(ctx, other)).unwrap();
                focus_in(events, other);
                assert_visible(ctx, events, src, true);
                assert!(events.eve_clients[&src].state.is_minimized());
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_new_and_recreated_previews_never_map_while_blocked() {
        use crate::daemon::handlers::state::hide_after_focus_loss;
        with_x11(|ctx| {
            ctx.conn
                .change_window_attributes(
                    ctx.screen.root,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
                )
                .unwrap()
                .check()
                .unwrap();
            for focus_hidden in [false, true] {
                let mut config = visibility_config();
                config.runtime_hidden = !focus_hidden;
                with_config(ctx, config, |events, _| {
                    if focus_hidden {
                        hide_after_focus_loss(events);
                    }
                    for (title, class) in [("EVE - Bob", "eve"), ("Custom Alice", "browser")] {
                        let src = window(ctx, title, class);
                        for creation in 0..3 {
                            if creation == 2 {
                                ctx.conn
                                    .change_property32(
                                        PropMode::REPLACE,
                                        ctx.screen.root,
                                        ctx.atoms.net_client_list,
                                        AtomEnum::WINDOW,
                                        &[src],
                                    )
                                    .unwrap()
                                    .check()
                                    .unwrap();
                                *events.eve_clients = super::scan_eve_windows(
                                    ctx,
                                    events.display_config,
                                    events.font_renderer,
                                    events.daemon_config,
                                    events.session_state,
                                    events.cycle_state,
                                    events.status_tx,
                                )
                                .unwrap();
                            } else {
                                handle_event(events, create_event(ctx, src)).unwrap();
                            }
                            assert_visible(ctx, events, src, false);
                            let preview = events.eve_clients[&src].window();
                            // The attribute round-trip above ensures all earlier map events are queued.
                            let mut drained = false;
                            for _ in 0..512 {
                                let Some(event) = ctx.conn.poll_for_event().unwrap() else {
                                    drained = true;
                                    break;
                                };
                                assert!(
                                    !matches!(event, Event::MapNotify(event) if event.window == preview),
                                    "blocked preview mapped during creation"
                                );
                            }
                            assert!(drained, "event drain exceeded its bound");
                            events.eve_clients.remove(&src);
                        }
                    }
                });
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_source_overrides_with_focus_hiding_disabled() {
        with_x11(|ctx| {
            let mut config = visibility_config();
            config.profile.thumbnail_hide_not_focused = false;
            config.profile.thumbnail_enabled = false;
            with_config(ctx, config, |events, _| {
                let src = window(ctx, "EVE - Bob", "eve");
                handle_event(events, create_event(ctx, src)).unwrap();
                assert_visible(ctx, events, src, true);
                swap_character(ctx, events, src, "Alice");
                assert_visible(ctx, events, src, false);
                let custom = window(ctx, "Custom Alice", "browser");
                handle_event(events, create_event(ctx, custom)).unwrap();
                assert_visible(ctx, events, custom, true);
                swap_character(ctx, events, src, "Bob");
                assert_visible(ctx, events, src, true);
                swap_character(ctx, events, src, "Charlie");
                assert_visible(ctx, events, src, false);
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_failed_map_requests_preserve_cached_state() {
        with_x11(|ctx| {
            for blocked in [false, true] {
                let mut config = visibility_config();
                config.runtime_hidden = blocked;
                with_config(ctx, config, |events, _| {
                    let src = window(ctx, "EVE - Bob", "eve");
                    handle_event(events, create_event(ctx, src)).unwrap();
                    let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                    ctx.conn
                        .destroy_window(thumbnail.window())
                        .unwrap()
                        .check()
                        .unwrap();
                    assert!(
                        thumbnail
                            .set_visibility_blocked(
                                !blocked,
                                events.display_config,
                                events.font_renderer
                            )
                            .is_err()
                    );
                    assert_eq!(thumbnail.is_visible(), !blocked);
                });
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_detection_recovers_focus_without_focus_in() {
        use crate::daemon::handlers::state::hide_after_focus_loss;
        with_x11(|ctx| {
            for already_tracked in [false, true] {
                for runtime_hidden in [false, true] {
                    let mut config = visibility_config();
                    config.runtime_hidden = runtime_hidden;
                    with_config(ctx, config, |events, _| {
                        let src = window(ctx, "EVE - Bob", "eve");
                        if already_tracked {
                            handle_event(events, create_event(ctx, src)).unwrap();
                        }
                        hide_after_focus_loss(events);
                        events.session_state.focus_loss_deadline = Some(std::time::Instant::now());
                        // Activation can precede event subscription; no FocusIn is delivered.
                        ctx.conn
                            .change_property32(
                                PropMode::REPLACE,
                                ctx.screen.root,
                                ctx.atoms.net_active_window,
                                AtomEnum::WINDOW,
                                &[src],
                            )
                            .unwrap()
                            .check()
                            .unwrap();
                        handle_event(events, create_event(ctx, src)).unwrap();
                        assert!(!events.session_state.focus_hidden);
                        assert!(events.session_state.focus_loss_deadline.is_none());
                        assert_visible(ctx, events, src, !runtime_hidden);
                        ctx.conn
                            .delete_property(ctx.screen.root, ctx.atoms.net_active_window)
                            .unwrap()
                            .check()
                            .unwrap();
                    });
                }
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_unchanged_block_does_not_repaint() {
        with_x11(|ctx| {
            with_config(ctx, visibility_config(), |events, _| {
                let src = window(ctx, "EVE - Bob", "eve");
                handle_event(events, create_event(ctx, src)).unwrap();
                assert_visible(ctx, events, src, true);
                let before = ctx.conn.get_input_focus().unwrap();
                let sequence = before.sequence_number();
                before.reply().unwrap();
                events
                    .eve_clients
                    .get_mut(&src)
                    .unwrap()
                    .set_visibility_blocked(false, events.display_config, events.font_renderer)
                    .unwrap();
                let after = ctx.conn.get_input_focus().unwrap();
                assert_eq!(
                    after.sequence_number(),
                    sequence + 1,
                    "unchanged visibility issued rendering requests"
                );
                after.reply().unwrap();
            })
        });
    }
}
