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

use super::preview_visibility::VisibilityContext;
use super::session_state::SessionState;
use super::source_registry::{SourceRegistry, TrackedSource};
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

    /// What the source registry admits this window as.
    pub fn tracked_source(&self) -> TrackedSource {
        match self.kind {
            SourceKind::Eve => TrackedSource::eve(self.name.clone()),
            SourceKind::Custom => TrackedSource::custom(self.name.clone()),
        }
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

    match_custom_rule(ctx, window, custom_rules)
}

/// Match configured custom-source rules against the window's current title and class.
/// Admitted custom sources use this directly, so an EVE-like title cannot reclassify them.
pub(super) fn match_custom_rule(
    ctx: &AppContext,
    window: Window,
    custom_rules: &[CustomWindowRule],
) -> Result<Option<WindowIdentity>> {
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
    // Callers register the candidate first; it counts even if no thumbnail is created.
    eve_client_count: usize,
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

    if display_config.hide_active {
        state.preview_visibility.mark_unconfirmed(window);
    }
    let visibility = VisibilityContext::new(display_config, daemon_config, state, eve_client_count);
    let blocked = state
        .preview_visibility
        .blocked(window, identity.kind, &visibility);
    let creation = (|| -> Result<Thumbnail<'a>> {
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
            blocked,
        )
        .context(format!(
            "Failed to create thumbnail for '{}' (window {})",
            character_name, window
        ))?;

        // Check minimized state
        let is_minimized = is_window_minimized(ctx.conn, window, ctx.atoms).unwrap_or(false);

        if is_minimized {
            thumbnail.minimized(display_config, font_renderer)?;
        }
        // The creation/startup border repaint and preview Expose supply the initial image.
        // Capture keeps fresh map-state/geometry guards for fleeting or unavailable sources.

        debug!(
            window = window,
            character = %character_name,
            is_custom = identity.is_custom(),
            "Created thumbnail"
        );
        Ok(thumbnail)
    })();
    if creation.is_err() {
        state.preview_visibility.creation_failed(window);
    }
    creation.map(Some)
}

// Initial scan for existing EVE clients and custom sources to populate thumbnails.

pub fn scan_eve_windows<'a>(
    ctx: &AppContext<'a>,
    display_config: &DisplayConfig,
    font_renderer: &crate::daemon::font::FontRenderer,
    daemon_config: &mut DaemonConfig,
    state: &mut SessionState,
    sources: &mut SourceRegistry,
    status_tx: &IpcSender<DaemonMessage>,
) -> Result<HashMap<Window, Thumbnail<'a>>> {
    let mut eve_clients = HashMap::new();

    // NOTE: Use _NET_CLIENT_LIST (EWMH) rather than query_tree(root) to get application
    // window IDs. Under reparenting WMs (e.g. KWin), query_tree(root) returns WM frame
    // windows whose properties (WM_CLASS, WM_NAME) don't match app rules, causing custom
    // sources and EVE clients to go undetected on daemon startup.
    let windows = crate::x11::get_client_list(ctx.conn, ctx.screen, ctx.atoms)
        .context("Failed to get window list via _NET_CLIENT_LIST")?;

    let mut detected_windows = Vec::new();
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

        // Register the identified window, even if no thumbnail is created
        sources.register(w, identity.tracked_source());
        detected_windows.push((w, identity));
    }

    // The whole initial population is registered before any preview can map, so the
    // first preview already knows whether it is the only EVE client.
    let eve_client_count = sources.eve_client_count();
    for (w, identity) in detected_windows {
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
            eve_client_count,
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
        source_registry::{SourceRegistry, TrackedSource},
    };
    use crate::{
        common::{
            ipc::DaemonMessage,
            types::{SourceIdentity, SourceKind},
        },
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
        protocol::{
            Event,
            damage::{ConnectionExt as _, NotifyEvent},
            xproto::*,
        },
        rust_connection::RustConnection,
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
        let mut sources = SourceRegistry::default();
        let mut drag = GroupDragState::default();
        let (tx, rx) = ipc::channel().unwrap();
        test(
            &mut EventContext {
                app_ctx: ctx,
                daemon_config: &mut config,
                eve_clients: &mut previews,
                session_state: &mut session,
                cycle_state: &mut cycle,
                sources: &mut sources,
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

    fn take_damage(ctx: &AppContext<'_>, damage: u32) -> Vec<NotifyEvent> {
        ctx.conn.get_input_focus().unwrap().reply().unwrap();
        let mut notifications = Vec::new();
        for _ in 0..1024 {
            let Some(event) = ctx.conn.poll_for_event().unwrap() else {
                return notifications;
            };
            match event {
                Event::DamageNotify(event) if event.damage == damage => notifications.push(event),
                Event::Error(error) => panic!("unexpected X11 error: {error:?}"),
                _ => {}
            }
        }
        panic!("damage fixture did not quiesce");
    }

    fn with_damage_source(
        overlap: bool,
        test: impl FnOnce(&mut EventContext<'_, '_>, &RustConnection, Window, Gcontext),
    ) {
        with_x11(|ctx| {
            // Keep real DAMAGE negotiation local to these tests, away from focus fixtures.
            ctx.conn
                .damage_query_version(1, 1)
                .unwrap()
                .reply()
                .unwrap();
            with_sources(ctx, |events, _| {
                let src = window(ctx, "YouTube", "browser");
                handle_event(events, create_event(ctx, src)).unwrap();
                if !overlap {
                    events
                        .eve_clients
                        .get_mut(&src)
                        .unwrap()
                        .reposition(700, 0)
                        .unwrap();
                }
                let damage = events.eve_clients[&src].damage();
                ctx.conn
                    .damage_subtract(damage, 0u32, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
                take_damage(ctx, damage);
                let (drawer, _) = x11rb::connect(None).unwrap();
                let gc = drawer.generate_id().unwrap();
                drawer
                    .create_gc(gc, src, &CreateGCAux::new())
                    .unwrap()
                    .check()
                    .unwrap();
                test(events, &drawer, src, gc);
                drawer.free_gc(gc).unwrap().check().unwrap();
            });
        });
    }

    fn draw_damage(drawer: &RustConnection, src: Window, gc: Gcontext, colors: &[u32]) {
        for &color in colors {
            drawer
                .change_gc(gc, &ChangeGCAux::new().foreground(color))
                .unwrap();
            drawer
                .poly_fill_rectangle(
                    src,
                    gc,
                    &[Rectangle {
                        x: 0,
                        y: 0,
                        width: 500,
                        height: 400,
                    }],
                )
                .unwrap();
        }
        drawer.get_input_focus().unwrap().reply().unwrap();
    }

    fn dispatch_damage(
        events: &mut EventContext<'_, '_>,
        event: NotifyEvent,
    ) -> anyhow::Result<()> {
        let result = handle_event(events, Event::DamageNotify(event));
        // A flush alone does not order our subtract against the drawer's next request.
        // Wait until it has been processed, even when a hidden/error path has no round trip.
        events
            .app_ctx
            .conn
            .get_input_focus()
            .unwrap()
            .reply()
            .unwrap();
        result
    }

    fn damage_preview_color(events: &EventContext<'_, '_>, src: Window) -> u32 {
        let thumbnail = &events.eve_clients[&src];
        let reply = events
            .app_ctx
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                thumbnail.window(),
                thumbnail.dimensions.width as i16 - 10,
                thumbnail.dimensions.height as i16 - 10,
                1,
                1,
                u32::MAX,
            )
            .unwrap()
            .reply()
            .unwrap();
        let bytes = reply.data.as_slice().try_into().unwrap();
        let pixel = if events.app_ctx.conn.setup().image_byte_order == ImageOrder::LSB_FIRST {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        };
        pixel & 0xFFFFFF
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_burst_coalesces_and_rearms() {
        for overlap in [false, true] {
            with_damage_source(overlap, |events, drawer, src, gc| {
                let damage = events.eve_clients[&src].damage();
                draw_damage(
                    drawer,
                    src,
                    gc,
                    &[0xFF0000, 0x0000FF, 0xFF0000, 0x0000FF, 0x00FF00],
                );
                let mut pending = take_damage(events.app_ctx, damage);
                assert_eq!(
                    pending.len(),
                    1,
                    "five draws must coalesce, overlap={overlap}"
                );
                dispatch_damage(events, pending.remove(0)).unwrap();
                assert_eq!(damage_preview_color(events, src), 0x00FF00);
                draw_damage(drawer, src, gc, &[0x0000FF]);
                let mut pending = take_damage(events.app_ctx, damage);
                assert_eq!(pending.len(), 1, "later draw must re-notify");
                dispatch_damage(events, pending.remove(0)).unwrap();
                assert_eq!(damage_preview_color(events, src), 0x0000FF);
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_hidden_preview_rearms_and_reveals() {
        with_damage_source(false, |events, drawer, src, gc| {
            let damage = events.eve_clients[&src].damage();
            events
                .eve_clients
                .get_mut(&src)
                .unwrap()
                .set_visibility_blocked(true, events.display_config, events.font_renderer)
                .unwrap();
            for color in [0xFF0000, 0x0000FF] {
                draw_damage(drawer, src, gc, &[color]);
                let mut pending = take_damage(events.app_ctx, damage);
                assert_eq!(pending.len(), 1);
                dispatch_damage(events, pending.remove(0)).unwrap();
                assert!(!events.eve_clients[&src].is_visible());
            }
            events
                .eve_clients
                .get_mut(&src)
                .unwrap()
                .set_visibility_blocked(false, events.display_config, events.font_renderer)
                .unwrap();
            assert_eq!(damage_preview_color(events, src), 0x0000FF);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_static_preview_skips_invalid_color_and_rearms() {
        with_damage_source(false, |events, drawer, src, gc| {
            let damage = events.eve_clients[&src].damage();
            draw_damage(drawer, src, gc, &[0x0000FF]);
            let mut pending = take_damage(events.app_ctx, damage);
            assert_eq!(pending.len(), 1);
            let mut invalid = events.display_config.clone();
            invalid
                .custom_source_settings
                .get_mut("YouTube")
                .unwrap()
                .preview_mode = crate::common::types::PreviewMode::Static {
                color: "invalid".into(),
            };
            let mut failing = EventContext {
                app_ctx: events.app_ctx,
                daemon_config: &mut *events.daemon_config,
                eve_clients: &mut *events.eve_clients,
                session_state: &mut *events.session_state,
                cycle_state: &mut *events.cycle_state,
                sources: &mut *events.sources,
                group_drag_state: &mut *events.group_drag_state,
                status_tx: events.status_tx,
                font_renderer: events.font_renderer,
                display_config: &invalid,
            };
            dispatch_damage(&mut failing, pending.remove(0)).unwrap();
            assert_eq!(
                failing
                    .eve_clients
                    .get_mut(&src)
                    .unwrap()
                    .update_for_damage(&invalid)
                    .unwrap(),
                crate::daemon::thumbnail::DamageUpdate::Static
            );
            assert!(events.eve_clients.contains_key(&src));
            draw_damage(drawer, src, gc, &[0x00FF00]);
            let mut pending = take_damage(events.app_ctx, damage);
            assert_eq!(
                pending.len(),
                1,
                "skipping a Static repaint must not stop damage notifications"
            );
            dispatch_damage(events, pending.remove(0)).unwrap();
            assert_eq!(damage_preview_color(events, src), 0x00FF00);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_id_is_authoritative_and_unknown_ids_are_ignored() {
        with_damage_source(false, |events, drawer, src, gc| {
            let damage = events.eve_clients[&src].damage();
            draw_damage(drawer, src, gc, &[0x0000FF]);
            let mut pending = take_damage(events.app_ctx, damage);
            assert_eq!(pending.len(), 1);
            let mut valid = pending.remove(0);
            let mut unknown = valid;
            unknown.damage = events.app_ctx.conn.generate_id().unwrap();
            dispatch_damage(events, unknown).unwrap();
            draw_damage(drawer, src, gc, &[0x00FF00]);
            assert!(
                take_damage(events.app_ctx, damage).is_empty(),
                "unknown ID must not subtract live damage"
            );
            assert_eq!(events.eve_clients[&src].damage(), damage);
            valid.drawable = events.app_ctx.conn.generate_id().unwrap();
            dispatch_damage(events, valid).unwrap();
            assert_eq!(damage_preview_color(events, src), 0x00FF00);
            draw_damage(drawer, src, gc, &[0x0000FF]);
            assert_eq!(take_damage(events.app_ctx, damage).len(), 1);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_destroyed_source_cleans_tracking() {
        with_damage_source(false, |events, drawer, src, gc| {
            let damage = events.eve_clients[&src].damage();
            events.session_state.update_window_position(src, 1, 2);
            events.session_state.update_last_character(src, "YouTube");
            draw_damage(drawer, src, gc, &[0x0000FF]);
            let mut pending = take_damage(events.app_ctx, damage);
            assert_eq!(pending.len(), 1);
            events
                .app_ctx
                .conn
                .destroy_window(src)
                .unwrap()
                .check()
                .unwrap();
            dispatch_damage(events, pending.remove(0)).unwrap();
            assert!(!events.eve_clients.contains_key(&src));
            assert!(!events.sources.contains(src));
            assert!(!events.session_state.window_positions.contains_key(&src));
            assert!(
                !events
                    .session_state
                    .window_last_character
                    .contains_key(&src)
            );
            events
                .app_ctx
                .conn
                .get_input_focus()
                .unwrap()
                .reply()
                .unwrap();
            while let Some(event) = events.app_ctx.conn.poll_for_event().unwrap() {
                if let Event::Error(error) = event {
                    // The server already freed these resources with the source window.
                    // Neither pipelined core query may leak a stale-resource error here.
                    assert!(
                        matches!(
                            (error.error_kind, error.minor_opcode),
                            (x11rb::protocol::ErrorKind::DamageBadDamage, 2 | 3)
                                | (x11rb::protocol::ErrorKind::RenderPicture, 7)
                        ),
                        "unexpected error after stale source cleanup: {error:?}"
                    );
                    assert!(![3, 14].contains(&error.major_opcode));
                }
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_unmapped_source_rearms_after_remap() {
        with_damage_source(false, |events, drawer, src, gc| {
            let damage = events.eve_clients[&src].damage();
            draw_damage(drawer, src, gc, &[0x0000FF]);
            let mut pending = take_damage(events.app_ctx, damage);
            assert_eq!(pending.len(), 1);
            events
                .app_ctx
                .conn
                .unmap_window(src)
                .unwrap()
                .check()
                .unwrap();
            dispatch_damage(events, pending.remove(0)).unwrap();
            assert!(events.eve_clients.contains_key(&src));
            assert!(take_damage(events.app_ctx, damage).is_empty());
            events
                .app_ctx
                .conn
                .map_window(src)
                .unwrap()
                .check()
                .unwrap();
            draw_damage(drawer, src, gc, &[0x00FF00]);
            let mut pending = take_damage(events.app_ctx, damage);
            assert_eq!(pending.len(), 1);
            dispatch_damage(events, pending.remove(0)).unwrap();
            assert_eq!(damage_preview_color(events, src), 0x00FF00);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_minimized_preserves_pixels_until_expose() {
        with_damage_source(false, |events, drawer, src, gc| {
            draw_damage(drawer, src, gc, &[0x0000FF]);
            let thumbnail = events.eve_clients.get_mut(&src).unwrap();
            thumbnail
                .minimized(events.display_config, events.font_renderer)
                .unwrap();
            let minimized = preview_image(events.app_ctx, thumbnail);
            let preview = thumbnail.window();
            let damage = thumbnail.damage();
            draw_damage(drawer, preview, gc, &[0xFF0000]);
            let witness = preview_image(events.app_ctx, &events.eve_clients[&src]);
            let pending = take_damage(events.app_ctx, damage);
            assert_eq!(pending.len(), 1);
            dispatch_damage(events, pending[0]).unwrap();
            assert!(
                preview_image(events.app_ctx, &events.eve_clients[&src]) == witness,
                "source damage must not repaint the Minimized presentation"
            );
            handle_event(
                events,
                Event::Expose(ExposeEvent {
                    response_type: EXPOSE_EVENT,
                    window: preview,
                    count: 0,
                    ..Default::default()
                }),
            )
            .unwrap();
            assert!(
                preview_image(events.app_ctx, &events.eve_clients[&src]) == minimized,
                "Expose must still restore the full Minimized presentation"
            );
            draw_damage(drawer, src, gc, &[0x00FF00]);
            assert_eq!(take_damage(events.app_ctx, damage).len(), 1);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn damage_unavailable_live_does_not_accumulate_overlay() {
        for unmapped in [false, true] {
            with_damage_source(false, |events, drawer, src, gc| {
                draw_damage(drawer, src, gc, &[0x0000FF]);
                let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                thumbnail
                    .border(events.display_config, false, false, events.font_renderer)
                    .unwrap();
                let before = preview_image(events.app_ctx, thumbnail);
                let pending = take_damage(events.app_ctx, thumbnail.damage());
                assert_eq!(pending.len(), 1);
                if unmapped {
                    events
                        .app_ctx
                        .conn
                        .unmap_window(src)
                        .unwrap()
                        .check()
                        .unwrap();
                } else {
                    events
                        .app_ctx
                        .conn
                        .configure_window(src, &ConfigureWindowAux::new().width(1).height(1))
                        .unwrap()
                        .check()
                        .unwrap();
                }
                // Repeated queued notifications exercise the unavailable path without relying
                // on whether a particular X server generates damage while a window is unmapped.
                for _ in 0..20 {
                    dispatch_damage(events, pending[0]).unwrap();
                }
                let thumbnail = &events.eve_clients[&src];
                assert!(
                    preview_image(events.app_ctx, thumbnail) == before,
                    "unmapped={unmapped}: unchanged overlay must not accumulate alpha"
                );
                thumbnail
                    .border(events.display_config, true, true, events.font_renderer)
                    .unwrap();
                assert!(
                    preview_image(events.app_ctx, thumbnail) != before,
                    "non-damage invalidation must still show a changed overlay on the frozen base"
                );
            });
        }
    }

    fn preview_image(
        ctx: &AppContext<'_>,
        thumbnail: &crate::daemon::thumbnail::Thumbnail<'_>,
    ) -> Vec<u8> {
        ctx.conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                thumbnail.window(),
                0,
                0,
                thumbnail.dimensions.width,
                thumbnail.dimensions.height,
                u32::MAX,
            )
            .unwrap()
            .reply()
            .unwrap()
            .data
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn first_eve_border_cleans_fleeting_source() {
        with_x11(|ctx| {
            ctx.conn
                .damage_query_version(1, 1)
                .unwrap()
                .reply()
                .unwrap();
            with_sources(ctx, |events, _| {
                let src = window(ctx, "EVE - Alice", "eve");
                let thumbnail = super::check_and_create_window(
                    events.app_ctx,
                    events.daemon_config,
                    events.display_config,
                    src,
                    events.font_renderer,
                    events.session_state,
                    events.eve_clients,
                    None,
                    1,
                )
                .unwrap()
                .unwrap();
                events.sources.register(src, TrackedSource::eve("Alice"));
                events.session_state.update_window_position(src, 1, 2);
                events.session_state.update_last_character(src, "Alice");
                events.eve_clients.insert(src, thumbnail);
                ctx.conn.destroy_window(src).unwrap().check().unwrap();
                crate::daemon::handlers::window::draw_initial_border(events, src).unwrap();
                assert!(!events.eve_clients.contains_key(&src));
                assert!(!events.sources.contains(src));
                assert!(!events.session_state.window_positions.contains_key(&src));
                assert!(
                    !events
                        .session_state
                        .window_last_character
                        .contains_key(&src)
                );
                ctx.conn.get_input_focus().unwrap().reply().unwrap();
                while let Some(event) = ctx.conn.poll_for_event().unwrap() {
                    if let Event::Error(error) = event {
                        assert!(
                            matches!(
                                (error.error_kind, error.minor_opcode),
                                (x11rb::protocol::ErrorKind::DamageBadDamage, 2)
                                    | (x11rb::protocol::ErrorKind::RenderPicture, 7)
                            ),
                            "unexpected first-border error: {error:?}"
                        );
                    }
                }
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn border_changes_repaint_idle_bases() {
        for mode in ["live", "static", "minimized", "hidden", "unmapped"] {
            with_damage_source(false, |events, drawer, src, gc| {
                let mut display = events.display_config.clone();
                display.inactive_border_enabled = false;
                display.active_border_size = 5;
                display.minimized_overlay_enabled = true;
                if mode == "static" {
                    display
                        .custom_source_settings
                        .get_mut("YouTube")
                        .unwrap()
                        .preview_mode = crate::common::types::PreviewMode::Static {
                        color: "#0000FF".into(),
                    };
                }
                draw_damage(drawer, src, gc, &[0x0000FF]);
                let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                thumbnail.update(&display, events.font_renderer).unwrap();
                thumbnail
                    .border(&display, true, true, events.font_renderer)
                    .unwrap();
                if mode == "minimized" {
                    thumbnail.minimized(&display, events.font_renderer).unwrap();
                    let before = preview_image(events.app_ctx, thumbnail);
                    thumbnail
                        .border(&display, true, true, events.font_renderer)
                        .unwrap();
                    assert!(
                        preview_image(events.app_ctx, thumbnail) == before,
                        "border requests must preserve minimized presentation"
                    );
                }
                if mode == "hidden" {
                    thumbnail
                        .set_visibility_blocked(true, &display, events.font_renderer)
                        .unwrap();
                }
                if mode == "unmapped" {
                    events
                        .app_ctx
                        .conn
                        .unmap_window(src)
                        .unwrap()
                        .check()
                        .unwrap();
                }
                thumbnail
                    .border(&display, false, false, events.font_renderer)
                    .unwrap();
                if mode == "hidden" {
                    thumbnail
                        .set_visibility_blocked(false, &display, events.font_renderer)
                        .unwrap();
                }
                let actual = preview_image(events.app_ctx, thumbnail);
                if mode == "unmapped" {
                    // No retained base exists: a removed opaque skip mark remains frozen.
                    events
                        .app_ctx
                        .conn
                        .map_window(src)
                        .unwrap()
                        .check()
                        .unwrap();
                }
                thumbnail.update(&display, events.font_renderer).unwrap();
                let restored = preview_image(events.app_ctx, thumbnail);
                if mode == "unmapped" {
                    assert_ne!(actual, restored, "remap finally restores the frozen base");
                } else {
                    assert!(
                        actual == restored,
                        "{mode}: border must restore the base without damage"
                    );
                }
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn identity_changes_preserve_focus_and_current_skip_style() {
        use crate::common::types::{SourceIdentity, ThumbnailState};
        with_x11(|ctx| {
            ctx.conn
                .damage_query_version(1, 1)
                .unwrap()
                .reply()
                .unwrap();
            with_sources(ctx, |events, _| {
                let src = window(ctx, "EVE - Alice", "eve");
                handle_event(events, create_event(ctx, src)).unwrap();
                let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                thumbnail.reposition(700, 0).unwrap();
                thumbnail.state = ThumbnailState::Normal { focused: true };
                events
                    .cycle_state
                    .toggle_skip(&SourceIdentity::eve("Alice"));
                thumbnail
                    .border(events.display_config, true, true, events.font_renderer)
                    .unwrap();
                swap_character(ctx, events, src, "Bob");
                let thumbnail = &events.eve_clients[&src];
                assert!(thumbnail.state.is_focused());
                let actual = preview_image(ctx, thumbnail);
                thumbnail
                    .border(events.display_config, true, false, events.font_renderer)
                    .unwrap();
                assert!(
                    preview_image(ctx, thumbnail) == actual,
                    "login must remove Alice's skip mark and keep focus"
                );

                events.cycle_state.toggle_skip(&SourceIdentity::eve("Bob"));
                thumbnail
                    .border(events.display_config, true, true, events.font_renderer)
                    .unwrap();
                set_title(ctx, src, crate::common::constants::eve::LOGGED_OUT_TITLE);
                handle_event(events, property_event(src, ctx.atoms.wm_name)).unwrap();
                let thumbnail = &events.eve_clients[&src];
                assert!(thumbnail.live_character_name().is_empty());
                assert_eq!(thumbnail.effective_character_name(), "Bob");
                assert!(thumbnail.state.is_focused());
                let actual = preview_image(ctx, thumbnail);
                thumbnail
                    .border(events.display_config, true, true, events.font_renderer)
                    .unwrap();
                assert!(
                    preview_image(ctx, thumbnail) == actual,
                    "logout must keep remembered skip/focus styling"
                );
            });
        });
    }

    fn take_exposes(ctx: &AppContext<'_>, preview: Window) -> Vec<ExposeEvent> {
        ctx.conn.get_input_focus().unwrap().reply().unwrap();
        let mut exposes = Vec::new();
        while let Some(event) = ctx.conn.poll_for_event().unwrap() {
            match event {
                Event::Expose(event) if event.window == preview => exposes.push(event),
                Event::Error(error) => panic!("unexpected X11 error: {error:?}"),
                _ => {}
            }
        }
        exposes
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn expose_preserves_unavailable_live_overlay() {
        for unmapped in [false, true] {
            with_damage_source(false, |events, drawer, src, gc| {
                let mut display = events.display_config.clone();
                display.active_border_size = 5;
                display.text_offset = crate::common::types::TextOffset::from_border_edge(10, 10);
                let settings = display.custom_source_settings.get_mut("YouTube").unwrap();
                settings.override_active_border_color = Some("#80FF0000".into());
                settings.override_text_color = Some("#80FFFFFF".into());
                draw_damage(drawer, src, gc, &[0x0000FF]);
                let thumbnail = events.eve_clients.get_mut(&src).unwrap();
                thumbnail
                    .border(&display, true, false, events.font_renderer)
                    .unwrap();
                let preview = thumbnail.window();
                // The border and label above the exposed rectangle must remain intact.
                let intact_bytes = usize::from(thumbnail.dimensions.width) * 4 * 30;
                let before = preview_image(events.app_ctx, thumbnail)[..intact_bytes].to_vec();
                take_exposes(events.app_ctx, preview);
                if unmapped {
                    events
                        .app_ctx
                        .conn
                        .unmap_window(src)
                        .unwrap()
                        .check()
                        .unwrap();
                } else {
                    events
                        .app_ctx
                        .conn
                        .configure_window(src, &ConfigureWindowAux::new().width(1).height(1))
                        .unwrap()
                        .check()
                        .unwrap();
                }
                for _ in 0..3 {
                    events
                        .app_ctx
                        .conn
                        .clear_area(true, preview, 40, 40, 1, 1)
                        .unwrap()
                        .check()
                        .unwrap();
                    let exposes = take_exposes(events.app_ctx, preview);
                    assert!(exposes.iter().any(|event| event.count == 0));
                    for expose in exposes {
                        handle_event(events, Event::Expose(expose)).unwrap();
                    }
                    let after = preview_image(events.app_ctx, &events.eve_clients[&src]);
                    assert!(
                        before == after[..intact_bytes],
                        "unmapped={unmapped}: Expose must not accumulate an unchanged overlay"
                    );
                }
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn expose_restores_covered_idle_previews() {
        for static_preview in [false, true] {
            with_damage_source(false, |events, drawer, src, gc| {
                let mut display = events.display_config.clone();
                if static_preview {
                    display
                        .custom_source_settings
                        .get_mut("YouTube")
                        .unwrap()
                        .preview_mode = crate::common::types::PreviewMode::Static {
                        color: "#0000FF".into(),
                    };
                }
                let mut events = EventContext {
                    app_ctx: events.app_ctx,
                    daemon_config: &mut *events.daemon_config,
                    eve_clients: &mut *events.eve_clients,
                    session_state: &mut *events.session_state,
                    cycle_state: &mut *events.cycle_state,
                    sources: &mut *events.sources,
                    group_drag_state: &mut *events.group_drag_state,
                    status_tx: events.status_tx,
                    font_renderer: events.font_renderer,
                    display_config: &display,
                };
                draw_damage(drawer, src, gc, &[0x00FF00]);
                events
                    .eve_clients
                    .get_mut(&src)
                    .unwrap()
                    .update(&display, events.font_renderer)
                    .unwrap();
                let preview = events.eve_clients[&src].window();
                take_exposes(events.app_ctx, preview);
                let expected = if static_preview { 0x0000FF } else { 0x00FF00 };
                assert_eq!(damage_preview_color(&events, src), expected);
                let geom = drawer.get_geometry(preview).unwrap().reply().unwrap();
                let cover = drawer.generate_id().unwrap();
                drawer
                    .create_window(
                        geom.depth,
                        cover,
                        events.app_ctx.screen.root,
                        geom.x,
                        geom.y,
                        geom.width,
                        geom.height,
                        0,
                        WindowClass::INPUT_OUTPUT,
                        events.app_ctx.screen.root_visual,
                        &CreateWindowAux::new()
                            .override_redirect(1)
                            .background_pixel(0xFF0000),
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                drawer.map_window(cover).unwrap().check().unwrap();
                drawer.unmap_window(cover).unwrap().check().unwrap();
                let exposes = take_exposes(events.app_ctx, preview);
                assert!(
                    exposes.iter().any(|event| event.count == 0),
                    "uncover must deliver a final Expose"
                );
                // No source damage is dispatched: this source is idle throughout uncovering.
                assert_ne!(
                    damage_preview_color(&events, src),
                    expected,
                    "fixture must lose preview contents on uncover"
                );
                for event in exposes {
                    handle_event(&mut events, Event::Expose(event)).unwrap();
                }
                assert_eq!(damage_preview_color(&events, src), expected);
                drawer.destroy_window(cover).unwrap().check().unwrap();
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn expose_ignores_intermediate_and_untracked_windows() {
        with_damage_source(false, |events, drawer, src, gc| {
            draw_damage(drawer, src, gc, &[0x00FF00]);
            let preview = events.eve_clients[&src].window();
            draw_damage(drawer, preview, gc, &[0x0000FF]);
            let mut expose = ExposeEvent {
                response_type: EXPOSE_EVENT,
                window: preview,
                count: 1,
                ..Default::default()
            };
            handle_event(events, Event::Expose(expose)).unwrap();
            assert_eq!(damage_preview_color(events, src), 0x0000FF);
            expose.count = 0;
            expose.window = src;
            handle_event(events, Event::Expose(expose)).unwrap();
            assert_eq!(damage_preview_color(events, src), 0x0000FF);
            expose.window = preview;
            handle_event(events, Event::Expose(expose)).unwrap();
            assert_eq!(damage_preview_color(events, src), 0x00FF00);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn expose_of_fleeting_source_cleans_tracking() {
        with_x11(|ctx| {
            ctx.conn
                .damage_query_version(1, 1)
                .unwrap()
                .reply()
                .unwrap();
            with_sources(ctx, |events, _| {
                let src = window(ctx, "YouTube", "browser");
                handle_event(events, create_event(ctx, src)).unwrap();
                let preview = events.eve_clients[&src].window();
                events.session_state.update_window_position(src, 1, 2);
                events.session_state.update_last_character(src, "YouTube");
                // Destroy before consuming the preview's original map Expose.
                ctx.conn.destroy_window(src).unwrap().check().unwrap();
                let exposes = take_exposes(ctx, preview);
                assert!(exposes.iter().any(|event| event.count == 0));
                for event in exposes {
                    handle_event(events, Event::Expose(event)).unwrap();
                }
                assert!(!events.eve_clients.contains_key(&src));
                assert!(!events.sources.contains(src));
                assert!(!events.session_state.window_positions.contains_key(&src));
                assert!(
                    !events
                        .session_state
                        .window_last_character
                        .contains_key(&src)
                );
                ctx.conn.get_input_focus().unwrap().reply().unwrap();
                while let Some(event) = ctx.conn.poll_for_event().unwrap() {
                    if let Event::Error(error) = event {
                        assert!(
                            matches!(
                                (error.error_kind, error.minor_opcode),
                                (x11rb::protocol::ErrorKind::DamageBadDamage, 2)
                                    | (x11rb::protocol::ErrorKind::RenderPicture, 7)
                            ),
                            "unexpected stale Expose error: {error:?}"
                        );
                    }
                }
            });
        });
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
                let sources_before = events.sources.clone();
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
                assert_eq!(*events.sources, sources_before);
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
        events
            .app_ctx
            .conn
            .set_input_focus(InputFocus::PARENT, src, 0u32)
            .unwrap()
            .check()
            .unwrap();
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
                ctx.conn
                    .set_input_focus(InputFocus::PARENT, ctx.screen.root, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
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
                // Model real minimization: the focused source is unmapped by the WM.
                ctx.conn.unmap_window(src).unwrap().check().unwrap();
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
                                    events.sources,
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
                            .set_input_focus(InputFocus::PARENT, src, 0u32)
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
    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn visibility_detection_uses_input_focus_even_when_wm_property_disagrees() {
        with_x11(|ctx| {
            with_config(ctx, visibility_config(), |events, _| {
                let src = window(ctx, "EVE - Bob", "eve");
                let outside = window(ctx, "Outside", "untracked");
                ctx.conn
                    .set_input_focus(InputFocus::PARENT, outside, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
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
                crate::daemon::handlers::state::hide_after_focus_loss(events);
                handle_event(events, create_event(ctx, src)).unwrap();
                assert!(events.session_state.focus_hidden);
                assert_visible(ctx, events, src, false);
                assert_eq!(events.cycle_state.get_current_window(), None);
                ctx.conn
                    .set_input_focus(InputFocus::PARENT, src, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn
                    .change_property32(
                        PropMode::REPLACE,
                        ctx.screen.root,
                        ctx.atoms.net_active_window,
                        AtomEnum::WINDOW,
                        &[outside],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                handle_event(events, create_event(ctx, src)).unwrap();
                assert!(!events.session_state.focus_hidden);
                assert_visible(ctx, events, src, true);
                assert_eq!(events.cycle_state.get_current_window(), Some(src));
            });
        });
    }

    fn hiding_config(single: bool, active: bool) -> DaemonConfig {
        DaemonConfig {
            profile: Profile {
                thumbnail_hide_when_single_client: single,
                thumbnail_hide_active: active,
                thumbnail_hide_not_focused: false,
                thumbnail_default_width: 160,
                thumbnail_default_height: 100,
                custom_windows: vec![
                    serde_json::from_value(serde_json::json!({
                        "alias": "Browser", "title_pattern": "Custom Browser", "limit": false
                    }))
                    .unwrap(),
                ],
                ..Profile::default()
            },
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        }
    }

    fn destroy_event(window: Window) -> Event {
        Event::DestroyNotify(DestroyNotifyEvent {
            response_type: DESTROY_NOTIFY_EVENT,
            event: window,
            window,
            ..Default::default()
        })
    }

    fn destroy_source(ctx: &AppContext<'_>, events: &mut EventContext<'_, '_>, window: Window) {
        ctx.conn.destroy_window(window).unwrap().check().unwrap();
        handle_event(events, destroy_event(window)).unwrap();
    }

    /// Round-trip first so every event caused by earlier requests is queued, then drain.
    fn queued_events(ctx: &AppContext<'_>) -> Vec<Event> {
        ctx.conn.get_input_focus().unwrap().reply().unwrap();
        let mut result = Vec::new();
        for _ in 0..4096 {
            let Some(event) = ctx.conn.poll_for_event().unwrap() else {
                return result;
            };
            result.push(event);
        }
        panic!("event queue did not drain");
    }

    fn assert_never_mapped(ctx: &AppContext<'_>, previews: &[Window]) {
        for event in queued_events(ctx) {
            assert!(
                !matches!(event, Event::MapNotify(event) if previews.contains(&event.window)),
                "a blocked preview mapped"
            );
        }
    }

    fn watch_root_structure(ctx: &AppContext<'_>) {
        ctx.conn
            .change_window_attributes(
                ctx.screen.root,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
            )
            .unwrap()
            .check()
            .unwrap();
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_startup_never_maps_sole_eve_preview() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            for single in [false, true] {
                for count in 0..=2 {
                    for custom_first in [false, true] {
                        let sources: Vec<_> = (0..count)
                            .map(|i| window(ctx, &format!("EVE - Pilot{i}"), "eve"))
                            .collect();
                        let custom = window(ctx, "Custom Browser", "browser");
                        let mut list = sources.clone();
                        list.insert(if custom_first { 0 } else { list.len() }, custom);
                        ctx.conn
                            .change_property32(
                                PropMode::REPLACE,
                                ctx.screen.root,
                                ctx.atoms.net_client_list,
                                AtomEnum::WINDOW,
                                &list,
                            )
                            .unwrap()
                            .check()
                            .unwrap();
                        queued_events(ctx);
                        with_config(ctx, hiding_config(single, false), |events, _| {
                            *events.eve_clients = super::scan_eve_windows(
                                ctx,
                                events.display_config,
                                events.font_renderer,
                                events.daemon_config,
                                events.session_state,
                                events.sources,
                                events.status_tx,
                            )
                            .unwrap();
                            assert_eq!(events.sources.eve_client_count(), count);
                            let hidden = single && count == 1;
                            for &source in &sources {
                                assert_visible(ctx, events, source, !hidden);
                            }
                            assert_visible(ctx, events, custom, true);
                            let blocked: Vec<_> = sources
                                .iter()
                                .filter(|_| hidden)
                                .map(|source| events.eve_clients[source].window())
                                .collect();
                            assert_never_mapped(ctx, &blocked);
                        });
                        for source in list {
                            ctx.conn.destroy_window(source).unwrap().check().unwrap();
                        }
                    }
                }
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_dynamic_counts_and_geometry() {
        with_x11(|ctx| {
            with_config(ctx, hiding_config(true, false), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                for event in detection_events(ctx, a) {
                    handle_event(events, event).unwrap();
                }
                assert_eq!(events.sources.eve_client_count(), 1);
                assert_visible(ctx, events, a, false);
                events
                    .eve_clients
                    .get_mut(&a)
                    .unwrap()
                    .reposition(100, 100)
                    .unwrap();
                let preview = events.eve_clients[&a].window();
                let dimensions = events.eve_clients[&a].dimensions;
                // Custom sources never count as EVE clients.
                let custom = window(ctx, "Custom Browser", "browser");
                handle_event(events, create_event(ctx, custom)).unwrap();
                assert_visible(ctx, events, custom, true);
                assert_visible(ctx, events, a, false);
                // A late-identified login screen counts once it is recognized.
                let b = window(ctx, "Not identified yet", "eve");
                handle_event(events, create_event(ctx, b)).unwrap();
                assert_eq!(events.sources.eve_client_count(), 1);
                set_title(ctx, b, "EVE");
                handle_event(events, property_event(b, ctx.atoms.wm_name)).unwrap();
                assert_eq!(events.sources.eve_client_count(), 2);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, true);
                // Login, logout, duplicate notifications, and minimizing keep the count.
                swap_character(ctx, events, b, "B");
                set_title(ctx, b, "EVE");
                handle_event(events, property_event(b, ctx.atoms.wm_name)).unwrap();
                for event in detection_events(ctx, b) {
                    handle_event(events, event).unwrap();
                }
                ctx.conn
                    .change_property32(
                        PropMode::REPLACE,
                        b,
                        ctx.atoms.net_wm_state,
                        AtomEnum::ATOM,
                        &[ctx.atoms.net_wm_state_hidden],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                handle_event(events, property_event(b, ctx.atoms.net_wm_state)).unwrap();
                ctx.conn.unmap_window(b).unwrap().check().unwrap();
                handle_event(
                    events,
                    Event::UnmapNotify(UnmapNotifyEvent {
                        response_type: UNMAP_NOTIFY_EVENT,
                        event: b,
                        window: b,
                        ..Default::default()
                    }),
                )
                .unwrap();
                assert_eq!(events.sources.eve_client_count(), 2);
                assert_visible(ctx, events, a, true);
                // Closing the second client hides the survivor without any focus event;
                // duplicate and preview-window destroy events are harmless.
                destroy_source(ctx, events, b);
                handle_event(events, destroy_event(b)).unwrap();
                handle_event(events, destroy_event(preview)).unwrap();
                assert_eq!(events.sources.eve_client_count(), 1);
                assert_visible(ctx, events, a, false);
                let c = window(ctx, "EVE - C", "eve");
                handle_event(events, create_event(ctx, c)).unwrap();
                assert_visible(ctx, events, a, true);
                assert_eq!(events.eve_clients[&a].window(), preview);
                assert_eq!(
                    events.eve_clients[&a].current_position,
                    crate::common::types::Position::new(100, 100)
                );
                assert_eq!(events.eve_clients[&a].dimensions, dimensions);
                destroy_source(ctx, events, a);
                destroy_source(ctx, events, c);
                assert_eq!(events.sources.eve_client_count(), 0);
                assert_visible(ctx, events, custom, true);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_counts_sources_without_thumbnails() {
        with_x11(|ctx| {
            let mut config = hiding_config(true, false);
            config.profile.thumbnail_enabled = false;
            config.profile.character_thumbnails.insert(
                "A".into(),
                crate::common::types::CharacterSettings {
                    override_render_preview: Some(true),
                    ..crate::common::types::CharacterSettings::new(100, 100, 160, 100)
                },
            );
            with_config(ctx, config, |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE", "eve");
                handle_event(events, create_event(ctx, a)).unwrap();
                assert_visible(ctx, events, a, false);
                handle_event(events, create_event(ctx, b)).unwrap();
                assert!(!events.eve_clients.contains_key(&b));
                assert_eq!(events.sources.eve_client_count(), 2);
                assert_visible(ctx, events, a, true);
                destroy_source(ctx, events, b);
                assert_visible(ctx, events, a, false);
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_stale_render_cleanup_hides_survivor() {
        with_x11(|ctx| {
            for via_expose in [false, true] {
                with_config(ctx, hiding_config(true, false), |events, _| {
                    let a = window(ctx, "EVE - A", "eve");
                    let b = window(ctx, "EVE - B", "eve");
                    for source in [a, b] {
                        handle_event(events, create_event(ctx, source)).unwrap();
                    }
                    assert_visible(ctx, events, a, true);
                    let damage = events.eve_clients[&b].damage();
                    let preview = events.eve_clients[&b].window();
                    ctx.conn.destroy_window(b).unwrap().check().unwrap();
                    // The render error, not DestroyNotify, is the first removal signal.
                    let event = if via_expose {
                        Event::Expose(ExposeEvent {
                            response_type: EXPOSE_EVENT,
                            window: preview,
                            width: 10,
                            height: 10,
                            ..Default::default()
                        })
                    } else {
                        Event::DamageNotify(NotifyEvent {
                            damage,
                            drawable: b,
                            ..Default::default()
                        })
                    };
                    handle_event(events, event).unwrap();
                    assert!(!events.sources.contains(b));
                    assert_eq!(events.sources.eve_client_count(), 1);
                    assert_visible(ctx, events, a, false);
                    handle_event(events, destroy_event(b)).unwrap();
                    assert_visible(ctx, events, a, false);
                    queued_events(ctx);
                });
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_parent_destruction_updates_count() {
        with_x11(|ctx| {
            with_config(ctx, hiding_config(true, false), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let frame = window(ctx, "Frame", "wm");
                let b = window(ctx, "EVE - B", "eve");
                ctx.conn
                    .reparent_window(b, frame, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                }
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, true);
                // Only the WM frame reports destruction; the parent match removes B.
                destroy_source(ctx, events, frame);
                assert_eq!(events.sources.eve_client_count(), 1);
                assert_visible(ctx, events, a, false);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_composes_with_manual_toggle() {
        use crate::daemon::handlers::state::toggle_previews;
        with_x11(|ctx| {
            with_config(ctx, hiding_config(true, false), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                handle_event(events, create_event(ctx, a)).unwrap();
                toggle_previews(events);
                // Opening a second client cannot bypass the manual block.
                let b = window(ctx, "EVE - B", "eve");
                handle_event(events, create_event(ctx, b)).unwrap();
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, false);
                toggle_previews(events);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, true);
                // Clearing the manual block cannot reveal a sole client.
                toggle_previews(events);
                destroy_source(ctx, events, b);
                toggle_previews(events);
                assert!(!events.daemon_config.runtime_hidden);
                assert_visible(ctx, events, a, false);
            })
        });
    }

    fn child_window(ctx: &AppContext<'_>, parent: Window) -> Window {
        let window = ctx.conn.generate_id().unwrap();
        ctx.conn
            .create_window(
                ctx.screen.root_depth,
                window,
                parent,
                0,
                0,
                10,
                10,
                0,
                WindowClass::INPUT_OUTPUT,
                ctx.screen.root_visual,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
        ctx.conn.map_window(window).unwrap().check().unwrap();
        window
    }

    /// Move real X input focus, as a window manager would, then observe it.
    fn focus_window(events: &mut EventContext<'_, '_>, window: Window) {
        events
            .app_ctx
            .conn
            .set_input_focus(InputFocus::PARENT, window, 0u32)
            .unwrap()
            .check()
            .unwrap();
        crate::daemon::activation::reconcile(events, std::time::Instant::now());
    }

    fn activation_requests(ctx: &AppContext<'_>) -> Vec<Window> {
        queued_events(ctx)
            .into_iter()
            .filter_map(|event| match event {
                Event::ClientMessage(event) if event.type_ == ctx.atoms.net_active_window => {
                    Some(event.window)
                }
                _ => None,
            })
            .collect()
    }

    fn pointer_event(preview: Window, button: u8, pressed: bool, state: KeyButMask) -> Event {
        let event = ButtonPressEvent {
            response_type: if pressed {
                BUTTON_PRESS_EVENT
            } else {
                BUTTON_RELEASE_EVENT
            },
            event: preview,
            detail: button,
            root_x: 110,
            root_y: 110,
            event_x: 10,
            event_y: 10,
            same_screen: true,
            state,
            ..Default::default()
        };
        if pressed {
            Event::ButtonPress(event)
        } else {
            Event::ButtonRelease(event)
        }
    }

    fn drag_motion(preview: Window, state: KeyButMask) -> Event {
        Event::MotionNotify(MotionNotifyEvent {
            response_type: MOTION_NOTIFY_EVENT,
            event: preview,
            root_x: 310,
            root_y: 330,
            state,
            ..Default::default()
        })
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_preview_follows_input_focus_not_the_wm_property() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, true), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                let custom = window(ctx, "Custom Browser", "browser");
                let outside = window(ctx, "Outside", "untracked");
                focus_window(events, outside);
                for source in [a, b, custom] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                    assert_visible(ctx, events, source, true);
                }
                focus_window(events, a);
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                let (pa, pb) = (
                    events.eve_clients[&a].window(),
                    events.eve_clients[&b].window(),
                );
                // A stale WM property naming A cannot override real input focus on B,
                // and the swap hides B's preview before revealing A's.
                ctx.conn
                    .change_property32(
                        PropMode::REPLACE,
                        ctx.screen.root,
                        ctx.atoms.net_active_window,
                        AtomEnum::WINDOW,
                        &[a],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                queued_events(ctx);
                focus_window(events, b);
                let order: Vec<_> = queued_events(ctx)
                    .into_iter()
                    .filter_map(|event| match event {
                        Event::UnmapNotify(event) if event.window == pb => Some("hide"),
                        Event::MapNotify(event) if event.window == pa => Some("reveal"),
                        _ => None,
                    })
                    .collect();
                assert_eq!(order, ["hide", "reveal"]);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, false);
                // Custom sources are current clients too.
                focus_window(events, custom);
                assert_visible(ctx, events, custom, false);
                assert_visible(ctx, events, b, true);
                // Focus on a preview overlay keeps the current block.
                focus_window(events, pb);
                assert_eq!(
                    events.session_state.preview_visibility.active_source(),
                    Some(custom)
                );
                assert_visible(ctx, events, custom, false);
                // Outside focus reveals everything while focus-loss hiding is off.
                focus_window(events, outside);
                for source in [a, b, custom] {
                    assert_visible(ctx, events, source, true);
                }
                ctx.conn
                    .delete_property(ctx.screen.root, ctx.atoms.net_active_window)
                    .unwrap()
                    .check()
                    .unwrap();
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_new_focused_client_never_maps_its_preview() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, true), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                handle_event(events, create_event(ctx, a)).unwrap();
                focus_window(events, a);
                assert_visible(ctx, events, a, false);
                // A newly launched client usually owns focus before it is detected.
                let b = window(ctx, "EVE - B", "eve");
                ctx.conn
                    .set_input_focus(InputFocus::PARENT, b, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
                queued_events(ctx);
                for event in detection_events(ctx, b) {
                    handle_event(events, event).unwrap();
                }
                assert_visible(ctx, events, b, false);
                assert_visible(ctx, events, a, true);
                assert_never_mapped(ctx, &[events.eve_clients[&b].window()]);
                // An unfocused newcomer appears as soon as focus is observed elsewhere.
                let c = window(ctx, "EVE - C", "eve");
                handle_event(events, create_event(ctx, c)).unwrap();
                assert_visible(ctx, events, c, true);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn preview_hiding_startup_never_maps_focused_framed_or_sole_previews() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            for framed in [false, true] {
                for single in [false, true] {
                    for active in [false, true] {
                        for count in 0..=2 {
                            let sources: Vec<_> = (0..count)
                                .map(|i| window(ctx, &format!("EVE - Pilot{i}"), "eve"))
                                .collect();
                            let custom = window(ctx, "Custom Browser", "browser");
                            let frame = window(ctx, "Frame", "wm");
                            let mut list = sources.clone();
                            list.push(custom);
                            let focused = sources.first().copied().unwrap_or(custom);
                            if framed {
                                ctx.conn
                                    .reparent_window(focused, frame, 0, 0)
                                    .unwrap()
                                    .check()
                                    .unwrap();
                            }
                            ctx.conn
                                .change_property32(
                                    PropMode::REPLACE,
                                    ctx.screen.root,
                                    ctx.atoms.net_client_list,
                                    AtomEnum::WINDOW,
                                    &list,
                                )
                                .unwrap()
                                .check()
                                .unwrap();
                            ctx.conn
                                .set_input_focus(
                                    InputFocus::PARENT,
                                    if framed { frame } else { focused },
                                    0u32,
                                )
                                .unwrap()
                                .check()
                                .unwrap();
                            queued_events(ctx);
                            with_config(ctx, hiding_config(single, active), |events, _| {
                                *events.eve_clients = super::scan_eve_windows(
                                    ctx,
                                    events.display_config,
                                    events.font_renderer,
                                    events.daemon_config,
                                    events.session_state,
                                    events.sources,
                                    events.status_tx,
                                )
                                .unwrap();
                                // The event loop observes focus before handling any event.
                                crate::daemon::activation::reconcile(
                                    events,
                                    std::time::Instant::now(),
                                );
                                let hidden = |source: Window| {
                                    (active && source == focused)
                                        || (single && count == 1 && source != custom)
                                };
                                for &source in &list {
                                    assert_visible(ctx, events, source, !hidden(source));
                                }
                                let blocked: Vec<_> = list
                                    .iter()
                                    .filter(|&&source| hidden(source))
                                    .map(|source| events.eve_clients[source].window())
                                    .collect();
                                assert_never_mapped(ctx, &blocked);
                            });
                            for window in list.into_iter().chain([frame]) {
                                ctx.conn.destroy_window(window).unwrap().check().unwrap();
                            }
                        }
                    }
                }
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_block_survives_pending_focus_loss_hide() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            let mut config = hiding_config(false, true);
            config.profile.thumbnail_hide_not_focused = true;
            with_config(ctx, config, |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                let outside = window(ctx, "Outside", "untracked");
                focus_window(events, a);
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                }
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                let pa = events.eve_clients[&a].window();
                queued_events(ctx);
                // Leaving EVE schedules the focus-loss hide; A must not flash meanwhile.
                focus_window(events, outside);
                let deadline = events.session_state.focus_loss_deadline.unwrap();
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                crate::daemon::activation::reconcile(events, deadline);
                assert!(events.session_state.focus_hidden);
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, false);
                assert_never_mapped(ctx, &[pa]);
                focus_window(events, b);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, false);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_observation_errors_preserve_blocks_and_deadlines() {
        use crate::daemon::handlers::state::toggle_previews;
        with_x11(|ctx| {
            let mut config = hiding_config(false, true);
            config.profile.thumbnail_hide_not_focused = true;
            with_config(ctx, config, |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                }
                focus_window(events, a);
                // Focus deeper than the ancestry bound below an untracked window is unknown.
                let mut deep = window(ctx, "Outside", "untracked");
                for _ in 0..12 {
                    deep = child_window(ctx, deep);
                }
                let expired = std::time::Instant::now();
                events.session_state.focus_loss_deadline = Some(expired);
                focus_window(events, deep);
                assert!(!events.session_state.focus_hidden);
                assert_eq!(events.session_state.focus_loss_deadline, Some(expired));
                assert_eq!(
                    events.session_state.preview_visibility.active_source(),
                    Some(a)
                );
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                // A client detected meanwhile stays hidden until focus is observed, even
                // through unrelated reconciliation such as a manual toggle round trip.
                watch_root_structure(ctx);
                queued_events(ctx);
                let c = window(ctx, "EVE - C", "eve");
                handle_event(events, create_event(ctx, c)).unwrap();
                toggle_previews(events);
                toggle_previews(events);
                assert_visible(ctx, events, c, false);
                assert_never_mapped(ctx, &[events.eve_clients[&c].window()]);
                focus_window(events, b);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, false);
                assert_visible(ctx, events, c, true);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn preview_hiding_reasons_compose_without_clearing_each_other() {
        use crate::daemon::handlers::state::toggle_previews;
        with_x11(|ctx| {
            with_config(ctx, hiding_config(true, true), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let outside = window(ctx, "Outside", "untracked");
                focus_window(events, outside);
                handle_event(events, create_event(ctx, a)).unwrap();
                assert_visible(ctx, events, a, false);
                focus_window(events, a);
                let b = window(ctx, "EVE - B", "eve");
                handle_event(events, create_event(ctx, b)).unwrap();
                // Two clients: only the current client's preview is hidden.
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                destroy_source(ctx, events, b);
                focus_window(events, outside);
                // No longer current, but still the sole client.
                assert_visible(ctx, events, a, false);
                let b = window(ctx, "EVE - B", "eve");
                handle_event(events, create_event(ctx, b)).unwrap();
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, true);
                toggle_previews(events);
                focus_window(events, b);
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, false);
                toggle_previews(events);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, false);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_hiding_clicks_use_the_pressed_source() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, true), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                    events
                        .eve_clients
                        .get_mut(&source)
                        .unwrap()
                        .reposition(100, 100)
                        .unwrap();
                }
                focus_window(events, a);
                let pa = events.eve_clients[&a].window();
                let pb = events.eve_clients[&b].window();
                queued_events(ctx);
                handle_event(events, pointer_event(pb, 1, true, KeyButMask::default())).unwrap();
                handle_event(events, pointer_event(pb, 1, false, KeyButMask::BUTTON1)).unwrap();
                assert_eq!(activation_requests(ctx), [b]);
                // A request alone is not confirmation.
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                focus_window(events, b);
                assert_visible(ctx, events, b, false);
                assert_visible(ctx, events, a, true);
                // Hiding the pressed preview ends its click.
                handle_event(events, pointer_event(pa, 1, true, KeyButMask::default())).unwrap();
                focus_window(events, a);
                assert!(events.session_state.pressed_preview_source.is_none());
                // Queued events for the hidden preview cannot target the one revealed below.
                queued_events(ctx);
                handle_event(events, pointer_event(pa, 1, true, KeyButMask::default())).unwrap();
                assert!(events.session_state.pressed_preview_source.is_none());
                for preview in [pa, pb, pb] {
                    handle_event(
                        events,
                        pointer_event(preview, 1, false, KeyButMask::BUTTON1),
                    )
                    .unwrap();
                }
                assert!(activation_requests(ctx).is_empty());
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_hiding_cancels_single_and_group_drags_without_saving() {
        with_x11(|ctx| {
            with_config(ctx, hiding_config(false, true), |events, rx| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                let outside = window(ctx, "Outside", "untracked");
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                    events
                        .eve_clients
                        .get_mut(&source)
                        .unwrap()
                        .reposition(100, 100)
                        .unwrap();
                }
                let pa = events.eve_clients[&a].window();
                for group in [false, true] {
                    focus_window(events, outside);
                    handle_event(events, pointer_event(pa, 3, true, KeyButMask::default()))
                        .unwrap();
                    let mut buttons = KeyButMask::BUTTON3;
                    if group {
                        handle_event(events, pointer_event(pa, 1, true, KeyButMask::BUTTON3))
                            .unwrap();
                        assert!(events.group_drag_state.is_active());
                        buttons |= KeyButMask::BUTTON1;
                    }
                    handle_event(events, drag_motion(pa, buttons)).unwrap();
                    let moving: &[Window] = if group { &[a, b] } else { &[a] };
                    for source in moving {
                        assert_ne!(
                            events.eve_clients[source].current_position,
                            crate::common::types::Position::new(100, 100)
                        );
                    }
                    drain_messages(rx);
                    focus_window(events, a);
                    assert!(!events.group_drag_state.is_active());
                    assert!(!events.eve_clients[&a].input_state.dragging);
                    for source in moving {
                        assert_eq!(
                            events.eve_clients[source].current_position,
                            crate::common::types::Position::new(100, 100)
                        );
                    }
                    handle_event(events, pointer_event(pa, 3, false, KeyButMask::BUTTON3)).unwrap();
                    if group {
                        handle_event(events, pointer_event(pa, 1, false, KeyButMask::BUTTON1))
                            .unwrap();
                    }
                    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
                }
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_hiding_completed_drags_save_without_activation() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, true), |events, rx| {
                let outside = window(ctx, "Outside", "untracked");
                focus_window(events, outside);
                let a = window(ctx, "EVE - A", "eve");
                handle_event(events, create_event(ctx, a)).unwrap();
                let preview = events.eve_clients[&a].window();
                for group in [false, true] {
                    events
                        .eve_clients
                        .get_mut(&a)
                        .unwrap()
                        .reposition(100, 100)
                        .unwrap();
                    drain_messages(rx);
                    queued_events(ctx);
                    handle_event(
                        events,
                        pointer_event(preview, 3, true, KeyButMask::default()),
                    )
                    .unwrap();
                    if group {
                        handle_event(events, pointer_event(preview, 1, true, KeyButMask::BUTTON3))
                            .unwrap();
                    }
                    handle_event(events, drag_motion(preview, KeyButMask::BUTTON3)).unwrap();
                    handle_event(
                        events,
                        pointer_event(preview, 3, false, KeyButMask::BUTTON3),
                    )
                    .unwrap();
                    if group {
                        handle_event(
                            events,
                            pointer_event(preview, 1, false, KeyButMask::BUTTON1),
                        )
                        .unwrap();
                    }
                    assert_eq!(
                        events.eve_clients[&a].current_position,
                        crate::common::types::Position::new(300, 320)
                    );
                    let DaemonMessage::PositionsChanged { updates } = rx.try_recv().unwrap() else {
                        panic!("expected spatial update");
                    };
                    assert_eq!(updates.len(), 1);
                    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
                    assert!(activation_requests(ctx).is_empty());
                }
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn destroyed_preview_cannot_retarget_queued_press() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, true), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                    events
                        .eve_clients
                        .get_mut(&source)
                        .unwrap()
                        .reposition(100, 100)
                        .unwrap();
                }
                let old_preview = events.eve_clients[&b].window();
                let survivor = events.eve_clients[&a].window();
                destroy_source(ctx, events, b);
                queued_events(ctx);
                handle_event(
                    events,
                    pointer_event(old_preview, 1, true, KeyButMask::default()),
                )
                .unwrap();
                assert!(events.session_state.pressed_preview_source.is_none());
                handle_event(
                    events,
                    pointer_event(survivor, 1, false, KeyButMask::BUTTON1),
                )
                .unwrap();
                assert!(activation_requests(ctx).is_empty());
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn single_client_count_change_cancels_drags_without_saving() {
        with_x11(|ctx| {
            with_config(ctx, hiding_config(true, false), |events, rx| {
                for group in [false, true] {
                    let a = window(ctx, "EVE - A", "eve");
                    let b = window(ctx, "EVE - B", "eve");
                    for source in [a, b] {
                        handle_event(events, create_event(ctx, source)).unwrap();
                        events
                            .eve_clients
                            .get_mut(&source)
                            .unwrap()
                            .reposition(100, 100)
                            .unwrap();
                    }
                    let pa = events.eve_clients[&a].window();
                    handle_event(events, pointer_event(pa, 3, true, KeyButMask::default()))
                        .unwrap();
                    let mut buttons = KeyButMask::BUTTON3;
                    if group {
                        handle_event(events, pointer_event(pa, 1, true, KeyButMask::BUTTON3))
                            .unwrap();
                        assert!(events.group_drag_state.is_active());
                        buttons |= KeyButMask::BUTTON1;
                    }
                    handle_event(events, drag_motion(pa, buttons)).unwrap();
                    drain_messages(rx);
                    // The second client closes mid-drag, so the dragged preview hides.
                    destroy_source(ctx, events, b);
                    assert_visible(ctx, events, a, false);
                    assert!(!events.group_drag_state.is_active());
                    assert!(!events.eve_clients[&a].input_state.dragging);
                    assert_eq!(
                        events.eve_clients[&a].current_position,
                        crate::common::types::Position::new(100, 100)
                    );
                    handle_event(events, pointer_event(pa, 3, false, KeyButMask::BUTTON3)).unwrap();
                    if group {
                        handle_event(events, pointer_event(pa, 1, false, KeyButMask::BUTTON1))
                            .unwrap();
                    }
                    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
                    destroy_source(ctx, events, a);
                }
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_focus_on_unrendered_source_keeps_manual_block() {
        use crate::daemon::handlers::state::toggle_previews;
        with_x11(|ctx| {
            let mut config = hiding_config(false, true);
            config.profile.thumbnail_enabled = false;
            config.profile.character_thumbnails.insert(
                "A".into(),
                crate::common::types::CharacterSettings {
                    override_render_preview: Some(true),
                    ..crate::common::types::CharacterSettings::new(100, 100, 160, 100)
                },
            );
            with_config(ctx, config, |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                let outside = window(ctx, "Outside", "untracked");
                focus_window(events, outside);
                for source in [a, b] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                }
                assert!(!events.eve_clients.contains_key(&b));
                assert_visible(ctx, events, a, true);
                toggle_previews(events);
                focus_window(events, b);
                assert_eq!(
                    events.session_state.preview_visibility.active_source(),
                    Some(b)
                );
                assert_visible(ctx, events, a, false);
                toggle_previews(events);
                assert_visible(ctx, events, a, true);
                focus_window(events, a);
                assert_visible(ctx, events, a, false);
                focus_window(events, b);
                assert_visible(ctx, events, a, true);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn active_hiding_follows_pointer_root_focus() {
        with_x11(|ctx| {
            with_config(ctx, hiding_config(false, true), |events, _| {
                let a = window(ctx, "EVE - A", "eve");
                let b = window(ctx, "EVE - B", "eve");
                for (source, x) in [(a, 0), (b, 600)] {
                    ctx.conn
                        .configure_window(
                            source,
                            &ConfigureWindowAux::new().x(x).y(0).width(300).height(300),
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                }
                let outside = window(ctx, "Outside", "untracked");
                focus_window(events, outside);
                for (source, y) in [(a, 450), (b, 600)] {
                    handle_event(events, create_event(ctx, source)).unwrap();
                    events
                        .eve_clients
                        .get_mut(&source)
                        .unwrap()
                        .reposition(1000, y)
                        .unwrap();
                }
                ctx.conn.destroy_window(outside).unwrap().check().unwrap();
                let point = |events: &mut EventContext<'_, '_>, x: i16, y: i16| {
                    events
                        .app_ctx
                        .conn
                        .warp_pointer(x11rb::NONE, events.app_ctx.screen.root, 0, 0, 0, 0, x, y)
                        .unwrap()
                        .check()
                        .unwrap();
                    crate::daemon::activation::reconcile(events, std::time::Instant::now());
                };
                // PointerRoot: keyboard input goes to whatever window is under the pointer.
                ctx.conn
                    .set_input_focus(InputFocus::POINTER_ROOT, 1u32, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
                point(events, 100, 100);
                assert_visible(ctx, events, a, false);
                assert_visible(ctx, events, b, true);
                point(events, 700, 100);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, false);
                // Passing over another preview keeps the current block.
                point(events, 1010, 460);
                assert_eq!(
                    events.session_state.preview_visibility.active_source(),
                    Some(b)
                );
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, false);
                // Bare desktop owns no client.
                point(events, 100, 700);
                assert_visible(ctx, events, a, true);
                assert_visible(ctx, events, b, true);
                point(events, 640, 400);
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn preview_click_release_respects_pixel_bounds() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, false), |events, _| {
                let source = window(ctx, "EVE - Edge", "eve");
                handle_event(events, create_event(ctx, source)).unwrap();
                let preview = events.eve_clients.get_mut(&source).unwrap();
                preview.reposition(100, 100).unwrap();
                let xid = preview.window();
                let right = 100 + preview.dimensions.width as i16;
                let bottom = 100 + preview.dimensions.height as i16;
                for (x, y, inside) in [
                    (100, 100, true),
                    (right - 1, bottom - 1, true),
                    (right, 110, false),
                    (110, bottom, false),
                    (99, 110, false),
                    (110, 99, false),
                ] {
                    queued_events(ctx);
                    handle_event(events, pointer_event(xid, 1, true, KeyButMask::default()))
                        .unwrap();
                    let Event::ButtonRelease(mut release) =
                        pointer_event(xid, 1, false, KeyButMask::BUTTON1)
                    else {
                        unreachable!()
                    };
                    release.root_x = x;
                    release.root_y = y;
                    handle_event(events, Event::ButtonRelease(release)).unwrap();
                    assert_eq!(
                        activation_requests(ctx),
                        if inside { vec![source] } else { vec![] },
                        "release at {x},{y}"
                    );
                }
                // A grab can deliver a press outside the preview. Returning inside for the
                // release must not turn that outside press into a preview click.
                let Event::ButtonPress(mut press) =
                    pointer_event(xid, 1, true, KeyButMask::default())
                else {
                    unreachable!()
                };
                press.root_x = right;
                handle_event(events, Event::ButtonPress(press)).unwrap();
                handle_event(events, pointer_event(xid, 1, false, KeyButMask::BUTTON1)).unwrap();
                assert!(activation_requests(ctx).is_empty());
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn preview_input_rejects_other_screen_coordinates() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, false), |events, _| {
                let source = window(ctx, "EVE - Click", "eve");
                handle_event(events, create_event(ctx, source)).unwrap();
                let preview = events.eve_clients.get_mut(&source).unwrap();
                preview.reposition(100, 100).unwrap();
                let xid = preview.window();
                queued_events(ctx);
                handle_event(events, pointer_event(xid, 1, true, KeyButMask::default())).unwrap();
                let Event::ButtonRelease(mut release) =
                    pointer_event(xid, 1, false, KeyButMask::BUTTON1)
                else {
                    unreachable!()
                };
                release.same_screen = false;
                handle_event(events, Event::ButtonRelease(release)).unwrap();
                assert!(activation_requests(ctx).is_empty());
                assert!(events.session_state.pressed_preview_source.is_none());

                // Root fallback is allowed only for coordinates on our actual root.
                for (event_window, root, same_screen, accepted) in [
                    (ctx.screen.root, ctx.screen.root, false, false),
                    (ctx.screen.root, source, true, false),
                    (xid, ctx.screen.root, false, false),
                    (ctx.screen.root, ctx.screen.root, true, true),
                ] {
                    let Event::ButtonPress(mut press) =
                        pointer_event(event_window, 1, true, KeyButMask::default())
                    else {
                        unreachable!()
                    };
                    press.root = root;
                    press.same_screen = same_screen;
                    handle_event(events, Event::ButtonPress(press)).unwrap();
                    assert_eq!(
                        events.session_state.pressed_preview_source,
                        accepted.then_some(source)
                    );
                }
                handle_event(events, pointer_event(xid, 1, false, KeyButMask::BUTTON1)).unwrap();
                assert_eq!(activation_requests(ctx), [source]);
            });
        });
    }

    /// The regular one-screen suite skips this body; CI explicitly enables it on two screens.
    #[test]
    #[ignore = "requires isolated two-screen Xvfb and EPM_X11_MULTISCREEN=1"]
    fn preview_cross_screen_release_does_not_activate() {
        use x11rb::protocol::xtest::ConnectionExt as _;

        struct RestorePointer<'a> {
            conn: &'a RustConnection,
            root: Window,
            x: i16,
            y: i16,
        }
        impl Drop for RestorePointer<'_> {
            fn drop(&mut self) {
                // X server pointer/button state outlives this test's connection. Restore
                // it even if an assertion fails, before another input test uses the server.
                if let Ok(cookie) =
                    self.conn
                        .xtest_fake_input(BUTTON_RELEASE_EVENT, 1, 0, self.root, 0, 0, 0)
                {
                    let _ = cookie.check();
                }
                if let Ok(cookie) =
                    self.conn
                        .warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, self.x, self.y)
                {
                    let _ = cookie.check();
                }
            }
        }

        if std::env::var("EPM_X11_MULTISCREEN").as_deref() != Ok("1") {
            return;
        }
        with_x11(|ctx| {
            let _restore = ctx
                .conn
                .setup()
                .roots
                .iter()
                .find_map(|screen| {
                    let pointer = ctx
                        .conn
                        .query_pointer(screen.root)
                        .unwrap()
                        .reply()
                        .unwrap();
                    if pointer.same_screen {
                        Some(RestorePointer {
                            conn: ctx.conn,
                            root: screen.root,
                            x: pointer.root_x,
                            y: pointer.root_y,
                        })
                    } else {
                        None
                    }
                })
                .expect("pointer is on a known X screen");
            let other_root = ctx
                .conn
                .setup()
                .roots
                .iter()
                .find(|screen| screen.root != ctx.screen.root)
                .expect("multiscreen test requires a second X screen")
                .root;
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, false), |events, _| {
                let source = window(ctx, "EVE - Click", "eve");
                handle_event(events, create_event(ctx, source)).unwrap();
                let preview = events.eve_clients.get_mut(&source).unwrap();
                preview.reposition(100, 100).unwrap();
                let xid = preview.window();
                ctx.conn
                    .warp_pointer(x11rb::NONE, xid, 0, 0, 0, 0, 10, 10)
                    .unwrap()
                    .check()
                    .unwrap();
                queued_events(ctx);
                ctx.conn
                    .xtest_fake_input(BUTTON_PRESS_EVENT, 1, 0, ctx.screen.root, 0, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
                let press = queued_events(ctx)
                    .into_iter()
                    .find_map(|event| {
                        if let Event::ButtonPress(press) = event {
                            Some(press)
                        } else {
                            None
                        }
                    })
                    .expect("real button press");
                assert_eq!(press.event, xid);
                assert!(press.same_screen);
                handle_event(events, Event::ButtonPress(press)).unwrap();
                ctx.conn
                    .warp_pointer(x11rb::NONE, other_root, 0, 0, 0, 0, 110, 110)
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn
                    .xtest_fake_input(BUTTON_RELEASE_EVENT, 1, 0, other_root, 0, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
                let release = queued_events(ctx)
                    .into_iter()
                    .find_map(|event| {
                        if let Event::ButtonRelease(release) = event {
                            Some(release)
                        } else {
                            None
                        }
                    })
                    .expect("real button release");
                assert_eq!(release.event, xid);
                assert_eq!(release.root, other_root);
                assert!(!release.same_screen);
                assert_eq!((release.root_x, release.root_y), (110, 110));
                handle_event(events, Event::ButtonRelease(release)).unwrap();
                assert!(activation_requests(ctx).is_empty());
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn identity_changes_cancel_input_before_saving_or_hiding() {
        with_x11(|ctx| {
            // A changing identity can be either the group anchor or another group member.
            for (group, other_anchor) in [(false, false), (true, false), (true, true)] {
                let mut config = visibility_config();
                config.profile.thumbnail_hide_not_focused = false;
                with_config(ctx, config, |events, rx| {
                    let source = window(ctx, "EVE - Bob", "eve");
                    let other = window(ctx, "Custom Alice", "browser");
                    for source in [source, other] {
                        handle_event(events, create_event(ctx, source)).unwrap();
                        events
                            .eve_clients
                            .get_mut(&source)
                            .unwrap()
                            .reposition(100, 100)
                            .unwrap();
                    }
                    let anchor = if other_anchor { other } else { source };
                    let preview = events.eve_clients[&anchor].window();
                    handle_event(
                        events,
                        pointer_event(preview, 3, true, KeyButMask::default()),
                    )
                    .unwrap();
                    let mut buttons = KeyButMask::BUTTON3;
                    if group {
                        handle_event(events, pointer_event(preview, 1, true, KeyButMask::BUTTON3))
                            .unwrap();
                        buttons |= KeyButMask::BUTTON1;
                    }
                    handle_event(events, drag_motion(preview, buttons)).unwrap();
                    let moved = events.eve_clients[&source].current_position;
                    assert_ne!(moved, crate::common::types::Position::new(100, 100));
                    // Same-name notifications do not interrupt the current gesture.
                    swap_character(ctx, events, source, "Bob");
                    assert_eq!(events.eve_clients[&source].current_position, moved);
                    assert_eq!(events.group_drag_state.is_active(), group);
                    assert_eq!(events.eve_clients[&source].input_state.dragging, !group);
                    swap_character(ctx, events, source, "Alice");
                    assert_visible(ctx, events, source, false);
                    assert!(!events.eve_clients[&source].input_state.dragging);
                    assert!(!events.group_drag_state.is_active());
                    assert_eq!(
                        events.daemon_config.character_thumbnails["Bob"].position(),
                        crate::common::types::Position::new(100, 100)
                    );
                    // Alice's saved position still takes precedence after Bob's drag is cancelled.
                    assert_eq!(
                        events.eve_clients[&source].current_position,
                        crate::common::types::Position::new(10, 20)
                    );
                    assert_eq!(
                        events.eve_clients[&other].current_position,
                        crate::common::types::Position::new(100, 100)
                    );
                    drain_messages(rx);
                    handle_event(
                        events,
                        pointer_event(preview, 3, false, KeyButMask::BUTTON3),
                    )
                    .unwrap();
                    if group {
                        handle_event(
                            events,
                            pointer_event(preview, 1, false, KeyButMask::BUTTON1),
                        )
                        .unwrap();
                    }
                    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
                });
            }
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn identity_change_cancels_click_even_when_the_new_preview_stays_visible() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, false), |events, _| {
                let source = window(ctx, "EVE - Bob", "eve");
                handle_event(events, create_event(ctx, source)).unwrap();
                events
                    .eve_clients
                    .get_mut(&source)
                    .unwrap()
                    .reposition(100, 100)
                    .unwrap();
                let preview = events.eve_clients[&source].window();
                queued_events(ctx);
                handle_event(
                    events,
                    pointer_event(preview, 1, true, KeyButMask::default()),
                )
                .unwrap();
                swap_character(ctx, events, source, "Bob");
                assert_eq!(events.session_state.pressed_preview_source, Some(source));
                swap_character(ctx, events, source, "Alice");
                assert_visible(ctx, events, source, true);
                assert!(events.session_state.pressed_preview_source.is_none());
                handle_event(
                    events,
                    pointer_event(preview, 1, false, KeyButMask::BUTTON1),
                )
                .unwrap();
                assert!(activation_requests(ctx).is_empty());
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn provisional_startup_reveal_preserves_minimized_rendering() {
        with_x11(|ctx| {
            watch_root_structure(ctx);
            with_config(ctx, hiding_config(false, true), |events, _| {
                let active = window(ctx, "EVE - Active", "eve");
                let minimized = window(ctx, "EVE - Minimized", "eve");
                ctx.conn
                    .change_property32(
                        PropMode::REPLACE,
                        minimized,
                        ctx.atoms.net_wm_state,
                        AtomEnum::ATOM,
                        &[ctx.atoms.net_wm_state_hidden],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn.unmap_window(minimized).unwrap().check().unwrap();
                ctx.conn
                    .change_property32(
                        PropMode::REPLACE,
                        ctx.screen.root,
                        ctx.atoms.net_client_list,
                        AtomEnum::WINDOW,
                        &[active, minimized],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn
                    .set_input_focus(InputFocus::PARENT, active, 0u32)
                    .unwrap()
                    .check()
                    .unwrap();
                queued_events(ctx);
                *events.eve_clients = super::scan_eve_windows(
                    ctx,
                    events.display_config,
                    events.font_renderer,
                    events.daemon_config,
                    events.session_state,
                    events.sources,
                    events.status_tx,
                )
                .unwrap();
                assert_visible(ctx, events, active, false);
                assert_visible(ctx, events, minimized, false);
                assert_never_mapped(
                    ctx,
                    &[
                        events.eve_clients[&active].window(),
                        events.eve_clients[&minimized].window(),
                    ],
                );
                crate::daemon::activation::reconcile(events, std::time::Instant::now());
                assert_visible(ctx, events, active, false);
                assert_visible(ctx, events, minimized, true);
                let thumbnail = events.eve_clients.get_mut(&minimized).unwrap();
                assert!(thumbnail.state.is_minimized());
                assert_eq!(
                    thumbnail.update_for_damage(events.display_config).unwrap(),
                    crate::daemon::thumbnail::DamageUpdate::Minimized
                );
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn failed_creation_drops_provisional_visibility_registration() {
        with_x11(|ctx| {
            let mut config = hiding_config(false, true);
            config.profile.thumbnail_default_width = 0;
            with_config(ctx, config, |events, _| {
                let source = window(ctx, "EVE - Failed", "eve");
                let identity = super::WindowIdentity::new_eve("Failed".into());
                events.sources.register(source, identity.tracked_source());
                let result = super::check_and_create_window(
                    ctx,
                    events.daemon_config,
                    events.display_config,
                    source,
                    events.font_renderer,
                    events.session_state,
                    events.eve_clients,
                    Some(identity),
                    1,
                );
                assert!(result.is_err());
                let visibility = super::VisibilityContext::new(
                    events.display_config,
                    events.daemon_config,
                    events.session_state,
                    1,
                );
                assert!(!events.session_state.preview_visibility.blocked(
                    source,
                    crate::common::types::SourceKind::Eve,
                    &visibility
                ));
            });
        });
    }

    fn map_event(ctx: &AppContext<'_>, window: Window) -> Event {
        Event::MapNotify(MapNotifyEvent {
            response_type: MAP_NOTIFY_EVENT,
            event: ctx.screen.root,
            window,
            ..Default::default()
        })
    }

    /// Fail if the Manager was told about an EVE character, then drain the rest.
    fn assert_no_eve_detected(rx: &IpcReceiver<DaemonMessage>) {
        for _ in 0..32 {
            match rx.try_recv() {
                Ok(DaemonMessage::CharacterDetected { name, is_custom }) => {
                    assert!(is_custom, "unexpected EVE character detection for {name}")
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => return,
                Err(error) => panic!("unexpected IPC error: {error}"),
            }
        }
        panic!("too many source registration messages");
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn eve_like_title_cannot_reclassify_custom_source() {
        // "Pilot" also covers a custom alias shared with a real EVE character.
        for alias in ["YouTube", "Pilot"] {
            with_x11(|ctx| {
                with_sources(ctx, |events, rx| {
                    events.daemon_config.profile.custom_windows[0].alias = alias.into();
                    let eve = window(ctx, "EVE - Pilot", "eve");
                    let custom = window(ctx, "YouTube", "browser");
                    handle_event(events, create_event(ctx, eve)).unwrap();
                    handle_event(events, create_event(ctx, custom)).unwrap();
                    drain_messages(rx);
                    let pilot = events.daemon_config.character_thumbnails["Pilot"].clone();
                    events
                        .eve_clients
                        .get_mut(&custom)
                        .unwrap()
                        .reposition(471, 251)
                        .unwrap();

                    set_title(ctx, custom, "EVE - Impostor");
                    handle_event(events, property_event(custom, ctx.atoms.wm_name)).unwrap();
                    handle_event(events, map_event(ctx, custom)).unwrap();

                    assert_eq!(events.sources.eve_client_count(), 1, "{alias}");
                    assert_eq!(
                        events
                            .sources
                            .get(custom)
                            .and_then(TrackedSource::live_identity),
                        Some(SourceIdentity::custom(alias))
                    );
                    let preview = &events.eve_clients[&custom];
                    assert_eq!(preview.source_kind(), SourceKind::Custom);
                    assert_eq!(preview.character_name, alias);
                    let saved = &events.daemon_config.character_thumbnails;
                    assert!(!saved.contains_key("Impostor") && !saved.contains_key("YouTube"));
                    assert_eq!((saved["Pilot"].x, saved["Pilot"].y), (pilot.x, pilot.y));
                    assert!(
                        !events
                            .session_state
                            .window_last_character
                            .contains_key(&custom)
                    );
                    assert_no_eve_detected(rx);
                });
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn custom_rule_title_cannot_reclassify_eve_client() {
        with_x11(|ctx| {
            with_sources(ctx, |events, _| {
                let eve = window(ctx, "EVE - Pilot", "eve");
                handle_event(events, create_event(ctx, eve)).unwrap();

                set_title(ctx, eve, "YouTube");
                handle_event(events, property_event(eve, ctx.atoms.wm_name)).unwrap();
                ctx.conn.unmap_window(eve).unwrap().check().unwrap();
                ctx.conn.map_window(eve).unwrap().check().unwrap();
                handle_event(events, map_event(ctx, eve)).unwrap();

                assert_eq!(events.sources.eve_client_count(), 1);
                assert_eq!(
                    events
                        .sources
                        .get(eve)
                        .and_then(TrackedSource::live_identity),
                    Some(SourceIdentity::eve("Pilot"))
                );
                let preview = &events.eve_clients[&eve];
                assert_eq!(preview.source_kind(), SourceKind::Eve);
                assert_eq!(preview.character_name, "Pilot");
                let custom = &events.daemon_config.custom_source_thumbnails;
                assert!(!custom.contains_key("Pilot") && !custom.contains_key("YouTube"));
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn map_applies_eve_rename_to_registry_and_preview() {
        with_x11(|ctx| {
            with_sources(ctx, |events, _| {
                let eve = window(ctx, "EVE - Pilot", "eve");
                handle_event(events, create_event(ctx, eve)).unwrap();

                // The WM_NAME notification has not been handled when the source maps.
                set_title(ctx, eve, "EVE - Other");
                handle_event(events, map_event(ctx, eve)).unwrap();

                let other = Some(SourceIdentity::eve("Other"));
                assert_eq!(
                    events
                        .sources
                        .get(eve)
                        .and_then(TrackedSource::live_identity),
                    other
                );
                assert_eq!(events.eve_clients[&eve].effective_source_identity(), other);
                assert!(
                    events
                        .daemon_config
                        .character_thumbnails
                        .contains_key("Other")
                );
                assert_eq!(events.session_state.window_last_character[&eve], "Other");
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn unrendered_sources_keep_admitted_kind_but_eve_names_update() {
        with_x11(|ctx| {
            with_sources(ctx, |events, rx| {
                let mut display = events.display_config.clone();
                display.enabled = false;
                let events = &mut EventContext {
                    display_config: &display,
                    app_ctx: events.app_ctx,
                    daemon_config: &mut *events.daemon_config,
                    eve_clients: &mut *events.eve_clients,
                    session_state: &mut *events.session_state,
                    cycle_state: &mut *events.cycle_state,
                    sources: &mut *events.sources,
                    group_drag_state: &mut *events.group_drag_state,
                    status_tx: events.status_tx,
                    font_renderer: events.font_renderer,
                };
                let eve = window(ctx, "EVE - Pilot", "eve");
                let custom = window(ctx, "YouTube", "browser");
                handle_event(events, create_event(ctx, eve)).unwrap();
                handle_event(events, create_event(ctx, custom)).unwrap();
                assert!(events.eve_clients.is_empty());
                drain_messages(rx);

                set_title(ctx, custom, "EVE - Impostor");
                set_title(ctx, eve, "EVE - Other");
                for source in [custom, eve] {
                    handle_event(events, property_event(source, ctx.atoms.wm_name)).unwrap();
                    handle_event(events, map_event(ctx, source)).unwrap();
                }

                assert_eq!(events.sources.eve_client_count(), 1);
                let registry = &events.sources;
                assert_eq!(
                    registry.get(custom),
                    Some(&TrackedSource::custom("YouTube"))
                );
                assert_eq!(registry.get(eve), Some(&TrackedSource::eve("Other")));
                let remembered = &events.session_state.window_last_character;
                assert!(!remembered.contains_key(&custom));
                assert_eq!(remembered[&eve], "Other");
                assert!(
                    !events
                        .daemon_config
                        .character_thumbnails
                        .contains_key("Impostor")
                );
            });
        });
    }
}
