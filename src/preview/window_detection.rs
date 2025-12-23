//! EVE window detection and thumbnail creation logic

use anyhow::{Context, Result};
use tracing::{debug, info};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::config::profile::CustomWindowRule;
use crate::config::DaemonConfig;
use crate::constants;
use crate::types::Dimensions;
use crate::x11::{get_window_class, is_window_eve, is_window_minimized, AppContext};
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
    // 1. Check if it's an EVE Client (Standard/Steam/Wine)
    // We reuse existing robust detection logic
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
    let wm_name_cookie = ctx.conn.get_property(
        false,
        window,
        ctx.atoms.wm_name,
        AtomEnum::STRING,
        0,
        1024,
    )?;
    
    let wm_class = get_window_class(ctx.conn, window, ctx.atoms)
        .ok()
        .flatten()
        .unwrap_or_default();

    let wm_name = if let Ok(reply) = wm_name_cookie.reply() {
        String::from_utf8_lossy(&reply.value).to_string()
    } else {
        String::new()
    };

    for rule in custom_rules {
        let matches_title = rule.title_pattern.as_ref()
            .map(|p| wm_name.to_lowercase().contains(&p.to_lowercase()))
            .unwrap_or(false); // If rule has title pattern, it MUST match
        
        let matches_class = rule.class_pattern.as_ref()
            .map(|p| wm_class.to_lowercase().contains(&p.to_lowercase()))
            .unwrap_or(false); // If rule has class pattern, it MUST match

        // Logic: Rule matches if...
        // - Title defined AND matches (AND Class is None OR matches)
        // - Class defined AND matches (AND Title is None OR matches)
        // Essentially, whatever criteria are defined must be satisfied.
        
        let mut matched = true;
        
        if let Some(_) = rule.title_pattern {
            if !matches_title { matched = false; }
        }
        if let Some(_) = rule.class_pattern {
            if !matches_class { matched = false; }
        }
        // If neither is defined, it's a catch-all? No, UI enforces at least one.
        if rule.title_pattern.is_none() && rule.class_pattern.is_none() {
            matched = false;
        }

        if matched {
            info!(
                window = window,
                alias = %rule.alias,
                title = %wm_name,
                class = %wm_class,
                "Identified Custom Source"
            );
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
        } else { None }
    } else { None };

    if let Some(pid) = pid {
        if pid == std::process::id() {
            return Ok(None);
        }
    }

    // 2. Title Verification
    ctx.conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;

    if let Some(eve_window) = is_window_eve(ctx.conn, window, ctx.atoms)? {
        let character_name = eve_window.character_name().to_string();
        
        info!(
            window = window,
            character = %character_name,
            "Confirmed EVE Client"
        );
        state.update_last_character(window, &character_name);
        
        ctx.conn.change_window_attributes(
            window,
             &ChangeWindowAttributesAux::new().event_mask(
                EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE | EventMask::STRUCTURE_NOTIFY
            ),
        )?;

        Ok(Some(character_name))
    } else {
        Ok(None)
    }
}

pub fn check_and_create_window<'a>(
    ctx: &AppContext<'a>,
    daemon_config: &DaemonConfig,
    window: Window,
    state: &mut SessionState,
    existing_thumbnails: &HashMap<Window, Thumbnail>,
) -> Result<Option<Thumbnail<'a>>> {
    // Check if window matches EVE or Custom Rule
    let identity = match identify_window(ctx, window, state, &daemon_config.profile.custom_windows)? {
        Some(id) => id,
        None => return Ok(None),
    };

    // Apply Limit Logic for Custom Sources
    if let Some(rule) = &identity.rule {
        if rule.limit {
            // Check if any EXISTING thumbnail has the same name
            // Note: existing_thumbnails contains previously processed windows
            if existing_thumbnails.values().any(|t| t.character_name == identity.name) {
                debug!(
                    window = window,
                    alias = %identity.name,
                    "Skipping duplicate custom source (limit enabled)"
                );
                return Ok(None);
            }
        }
    }

    // Skip thumbnail creation if thumbnails are disabled (but window is still tracked via identity/cycle potentially?)
    // Actually, if we return None here, it might not get added to cycle state?
    // In original code, returning None means NO thumbnail.
    // Cycle state registration is done in `scan_eve_windows` for initial list, 
    // and `handle_create_notify` calls `identify_window` SEPARATELY before calling this.
    // So `check_and_create_window` is strictly for THUMBNAIL creation.
    
    if !ctx.config.enabled {
        return Ok(None);
    }

    let character_name = identity.name;

    // Get saved position and dimensions
    let position = state.get_position(
        &character_name,
        window,
        &daemon_config.character_thumbnails,
        daemon_config.profile.thumbnail_preserve_position_on_swap,
    );

    // Get dimensions: From settings, OR from Rule (if custom), OR default
    let (dimensions, preview_mode) = if let Some(settings) =
        daemon_config.character_thumbnails.get(&character_name)
    {
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
        (dims, settings.preview_mode.clone())
    } else {
        // No saved settings
        if let Some(rule) = identity.rule {
            // Use Custom Rule defaults
            (Dimensions::new(rule.default_width, rule.default_height), crate::types::PreviewMode::default())
        } else {
             // Auto-detect EVE default
             let (w, h) = daemon_config
                .default_thumbnail_size(ctx.screen.width_in_pixels, ctx.screen.height_in_pixels);
            (Dimensions::new(w, h), crate::types::PreviewMode::default())
        }
    };

    let mut thumbnail = Thumbnail::new(
        ctx,
        character_name.clone(),
        window,
        ctx.font_renderer,
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
        thumbnail.minimized()?;
    } else {
        // Removed forced update to prevent crashes with fleeting windows (e.g. Firefox popups).
        // Standard X11 Damage events will trigger the first update naturally.
    }
    
    info!(
        window = window,
        character = %character_name,
        is_custom = !identity.is_eve,
        "Created thumbnail"
    );
    Ok(Some(thumbnail))
}

/// Initial scan for existing EVE windows to populate thumbnails
pub fn scan_eve_windows<'a>(
    ctx: &AppContext<'a>,
    daemon_config: &mut DaemonConfig,
    state: &mut SessionState,
) -> Result<HashMap<Window, Thumbnail<'a>>> {
    let net_client_list = ctx.atoms.net_client_list;
    let prop = ctx
        .conn
        .get_property(
            false,
            ctx.screen.root,
            net_client_list,
            AtomEnum::WINDOW,
            0,
            u32::MAX,
        )
        .context("Failed to query _NET_CLIENT_LIST property")?
        .reply()
        .context("Failed to get window list from X11 server")?;
    let windows: Vec<u32> = prop
        .value32()
        .ok_or_else(|| anyhow::anyhow!("Invalid return from _NET_CLIENT_LIST"))?
        .collect();

    let mut eve_clients = HashMap::new();
    for w in windows {
        // Use the map we are building as the "existing_thumbnails" context for limit checks
        // We handle errors gracefully here so one bad window doesn't prevent the daemon from starting
        match check_and_create_window(ctx, daemon_config, w, state, &eve_clients) {
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
                            let settings = crate::types::CharacterSettings::new(
                                geom.x,
                                geom.y,
                                eve.dimensions.width,
                                eve.dimensions.height,
                            );
                            daemon_config
                                .character_thumbnails
                                .insert(eve.character_name.clone(), settings);
                        }
                    }
                    Err(e) => {
                         tracing::warn!("Failed to query geometry for new thumbnail window {}: {}", eve.window(), e);
                         // Continue anyway, we just won't update the saved position
                    }
                }

                eve_clients.insert(w, eve);
            }
            Ok(None) => {
                // Window ignored / not matched
            }
            Err(e) => {
                tracing::warn!("Failed to process window {} during initial scan: {}", w, e);
            }
        }
    }

    // Save once after processing all windows (avoids repeated disk writes)
    if daemon_config.profile.thumbnail_auto_save_position && !eve_clients.is_empty() {
        daemon_config
            .save()
            .context("Failed to save initial positions after startup scan")?;
    }

    ctx.conn
        .flush()
        .context("Failed to flush X11 connection after creating thumbnails")?;
    Ok(eve_clients)
}
