//! EVE window detection and thumbnail creation logic

use anyhow::{Context, Result};
use tracing::debug;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::common::constants;
use crate::common::types::Dimensions;
use crate::config::DaemonConfig;
use crate::config::DisplayConfig;
use crate::config::profile::CustomWindowRule;
use crate::x11::{AppContext, get_window_class, is_window_eve, is_window_minimized};
use std::collections::HashMap;

use super::session_state::SessionState;
use super::thumbnail::Thumbnail;

/// Check if a window is an EVE client and return its character name
/// Returns Some(character_name) for EVE windows, None for non-EVE windows
#[derive(Debug, Clone)]
pub struct WindowIdentity {
    pub name: String,
    pub is_eve: bool,
    pub rule: Option<CustomWindowRule>,
}

/// Identify a window as either an EVE client or a Custom Source
pub fn identify_window(
    ctx: &AppContext,
    window: Window,
    state: &mut SessionState,
    custom_rules: &[CustomWindowRule],
) -> Result<Option<WindowIdentity>> {
    // Check for EVE Client identity first (Standard/Steam/Wine) using robust detection
    if let Some(eve_window) = check_eve_window_internal(ctx, window, state)? {
        let name = eve_window;
        return Ok(Some(WindowIdentity {
            name,
            is_eve: true,
            rule: None,
        }));
    }

    // 2. Check Custom Rules
    // Get window properties once to avoid repeated round-trips
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
            debug!(
                window = window,
                alias = %rule.alias,
                title = %wm_name,
                class = %wm_class,
                "Identified Custom Source"
            );
            state.update_last_character(window, &rule.alias);
            return Ok(Some(WindowIdentity {
                name: rule.alias.clone(),
                is_eve: false,
                rule: Some(rule.clone()),
            }));
        }
    }

    Ok(None)
}

/// Internal helper to check EVE specifics (extracted from original check_eve_window)
fn check_eve_window_internal(
    ctx: &AppContext,
    window: Window,
    state: &mut SessionState,
) -> Result<Option<String>> {
    // 1. Get PID (Optimization to skip own windows)
    let pid_atom = ctx.atoms.net_wm_pid;
    let pid = if let Ok(prop) = ctx
        .conn
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .context(format!("Failed to query _NET_WM_PID for {}", window))?
        .reply()
    {
        if !prop.value.is_empty() {
            Some(u32::from_ne_bytes(
                prop.value[0..constants::x11::PID_PROPERTY_SIZE]
                    .try_into()
                    .unwrap_or([0; 4]),
            ))
        } else {
            None
        }
    } else {
        None
    };

    // Skip our own windows to avoid recursion
    if pid.is_some_and(|p| p == std::process::id()) {
        return Ok(None);
    }

    // 2. Title Verification
    ctx.conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;

    if let Some(eve_window) = is_window_eve(ctx.conn, window, ctx.atoms)? {
        let character_name = eve_window.character_name().to_string();

        debug!(
            window = window,
            character = %character_name,
            "Confirmed EVE Client"
        );
        state.update_last_character(window, &character_name);

        ctx.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE | EventMask::STRUCTURE_NOTIFY,
            ),
        )?;

        Ok(Some(character_name))
    } else {
        Ok(None)
    }
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
    if !identity.is_eve {
        // FILTER 1: Must be mapped and viewable OR minimized
        // We removed the strict MapState::VIEWABLE check to allow minimized windows to be detected.
        // Utility windows are still filtered by `is_normal_window` below.

        if !crate::x11::is_normal_window(ctx.conn, window, ctx.atoms).unwrap_or(true) {
            debug!(window = window, alias = %identity.name, "Skipping non-normal custom source (utility/dock)");
            return Ok(None);
        }

        // IMPORTANT: Register for events on this custom source window!
        // This is done for EVE windows inside check_eve_window_internal, but we must do it here for custom sources.
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
            .any(|t| t.character_name == identity.name)
        {
            debug!(
                window = window,
                alias = %identity.name,
                "Skipping duplicate custom source (limit enabled)"
            );
            return Ok(None);
        }
    }

    // Cycle state registration is handled separately in `scan_eve_windows` for the initial list
    // and `handle_create_notify` calls `identify_window` before calling this.
    // This function is strictly for determining if we should create a renderable thumbnail.

    let character_name = identity.name;

    // Get saved position and dimensions
    // Determine which map to query based on identity type
    let settings_map = if identity.is_eve {
        &daemon_config.character_thumbnails
    } else {
        &daemon_config.custom_source_thumbnails
    };

    let profile_map = if identity.is_eve {
        &daemon_config.profile.character_thumbnails
    } else {
        &daemon_config.profile.custom_source_thumbnails
    };

    // Priority 1: Runtime Settings (active session changes)
    // Priority 2: Profile Settings (saved on disk)
    // Priority 3: Inheritance / Session State
    let position = if let Some(settings) = settings_map.get(&character_name) {
        Some(settings.position())
    } else if let Some(settings) = profile_map.get(&character_name) {
        Some(settings.position())
    } else {
        // Pass empty map to enforce inheritance/fallback logic only
        state.get_position(
            &character_name,
            window,
            &HashMap::new(),
            daemon_config.profile.thumbnail_preserve_position_on_swap,
        )
    };

    // Determine effective settings for dimensions and mode
    let effective_settings = settings_map
        .get(&character_name)
        .or_else(|| profile_map.get(&character_name));

    // Check for "Always Show" override
    let force_enable = effective_settings
        .and_then(|s| s.override_render_preview)
        .unwrap_or(false);

    if !display_config.enabled && !force_enable {
        return Ok(None);
    }

    // Determine effective settings for dimensions and mode
    let effective_settings = settings_map
        .get(&character_name)
        .or_else(|| profile_map.get(&character_name));

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
        if let Some(rule) = identity.rule {
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
        character_name.clone(),
        window,
        display_config,
        font_renderer,
        position,
        dimensions,
        preview_mode,
    )
    .context(format!(
        "Failed to create thumbnail for '{}' (window {})",
        character_name, window
    ))?;

    // Check minimized state
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
        is_custom = !identity.is_eve,
        "Created thumbnail"
    );
    Ok(Some(thumbnail))
}

/// Initial scan for existing EVE windows to populate thumbnails
use super::cycle_state::CycleState;

pub fn scan_eve_windows<'a>(
    ctx: &AppContext<'a>,
    display_config: &DisplayConfig,
    font_renderer: &crate::daemon::font::FontRenderer,
    daemon_config: &mut DaemonConfig,
    state: &mut SessionState,
    cycle_state: &mut CycleState,
) -> Result<HashMap<Window, Thumbnail<'a>>> {
    let mut eve_clients = HashMap::new();

    // Get all windows
    let windows = match ctx.conn.query_tree(ctx.screen.root) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply.children,
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to get root window children: {}", e));
            }
        },
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to query tree: {}", e));
        }
    };

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
        cycle_state.add_window(identity.name.clone(), w);

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
                        // Update character_thumbnails in memory (skip logged-out clients with empty name)
                        if !eve.character_name.is_empty() {
                            let settings = crate::common::types::CharacterSettings::new(
                                geom.x,
                                geom.y,
                                eve.dimensions.width,
                                eve.dimensions.height,
                            );

                            // Route settings to the correct map based on whether this alias matches a Custom Rule.
                            // This ensures separation even if originally detected as a generic client.
                            let is_custom_alias = daemon_config
                                .profile
                                .custom_windows
                                .iter()
                                .any(|r| r.alias == eve.character_name);

                            if is_custom_alias {
                                // NOTE: specific check to preserve existing overrides (like preview_mode)
                                // if they were already loaded from the profile config key.
                                if let Some(existing) = daemon_config
                                    .custom_source_thumbnails
                                    .get_mut(&eve.character_name)
                                {
                                    existing.x = settings.x;
                                    existing.y = settings.y;
                                    existing.dimensions = settings.dimensions;
                                } else {
                                    daemon_config
                                        .custom_source_thumbnails
                                        .insert(eve.character_name.clone(), settings);
                                }
                            } else if let Some(existing) = daemon_config
                                .character_thumbnails
                                .get_mut(&eve.character_name)
                            {
                                existing.x = settings.x;
                                existing.y = settings.y;
                                existing.dimensions = settings.dimensions;
                            } else {
                                daemon_config
                                    .character_thumbnails
                                    .insert(eve.character_name.clone(), settings);
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
                // Window ignored / not matched for thumbnail creation (e.g. global disabled)
                // BUT it was already added to identified_windows above
                // (actually it was added to CycleState above)
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
