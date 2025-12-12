//! EVE window detection and thumbnail creation logic

use anyhow::{Context, Result};
use tracing::{debug, info};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::config::DaemonConfig;
use crate::constants::{self, paths, wine};
use crate::types::Dimensions;
use crate::x11::{
    AppContext, get_window_class, is_eve_window_class, is_window_eve, is_window_minimized,
};
use std::collections::HashMap;

use super::session_state::SessionState;
use super::thumbnail::Thumbnail;

/// Check if a window is an EVE client and return its character name
/// Returns Some(character_name) for EVE windows, None for non-EVE windows
pub fn check_eve_window(
    ctx: &AppContext,
    window: Window,
    state: &mut SessionState,
) -> Result<Option<String>> {
    // 1. Get Window Class
    let class_name = get_window_class(ctx.conn, window, ctx.atoms)
        .ok() // Ignore errors
        .flatten();
    let is_known_class = class_name
        .as_ref()
        .map(|c| is_eve_window_class(c))
        .unwrap_or(false);

    // 2. Get PID
    let pid_atom = ctx.atoms.net_wm_pid;
    let pid = if let Ok(prop) = ctx
        .conn
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .context(format!(
            "Failed to query _NET_WM_PID property for window {}",
            window
        ))?
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

    // 3. Evaluate Process Strategy
    let should_inspect_title = if let Some(pid) = pid {
        // Skip our own windows (thumbnails)
        if pid == std::process::id() {
            return Ok(None);
        }

        if is_wine_process(pid) {
            debug!(
                window = window,
                pid = pid,
                "Identified Verified Wine Process"
            );
            true
        } else if is_known_class {
            // NOTE: PID check failed (likely namespace mismatch e.g., PID 2), but Class matched.
            // We explicitly accept this as a container/Flatpak fallback.
            debug!(
                window = window,
                class = ?class_name,
                pid = pid,
                "Identified Container/Flatpak process (PID verification failed, but WM_CLASS matched)"
            );
            true
        } else {
            false
        }
    } else {
        // Edge case: No PID, but Class matches.
        if is_known_class {
            debug!(window = window, class = ?class_name, "No PID, but WM_CLASS matched");
            true
        } else {
            false
        }
    };

    if !should_inspect_title {
        return Ok(None);
    }

    // 4. Final Gate: Title Verification
    // NOTE: Strictly require "EVE - " title to avoid false positives (e.g. other steam_app_0 games)

    // Set event mask to ensure we can read properties reliably
    ctx.conn
        .change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .context(format!("Failed to set event mask for window {}", window))?;

    if let Some(eve_window) = is_window_eve(ctx.conn, window, ctx.atoms).context(format!(
        "Failed to check if window {} is EVE client",
        window
    ))? {
        let character_name = eve_window.character_name().to_string();

        info!(
            window = window,
            character = %character_name,
            class = ?class_name,
            "Confirmed EVE Client (Title Verified)"
        );

        // Track last known character for this window (for logged-out cycling feature)
        state.update_last_character(window, &character_name);

        ctx.conn
            .change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE),
            )
            .context(format!(
                "Failed to set focus event mask for EVE window {} ('{}')",
                window, character_name
            ))?;

        Ok(Some(character_name))
    } else {
        // Title verification failed
        // NOTE: It might be a valid Steam app (steam_app_0) but NOT EVE.
        debug!(
            window = window,
            class = ?class_name,
            "Window matched process/class criteria but failed EVE title verification"
        );
        Ok(None)
    }
}

pub fn check_and_create_window<'a>(
    ctx: &AppContext<'a>,
    daemon_config: &DaemonConfig,
    window: Window,
    state: &mut SessionState,
) -> Result<Option<Thumbnail<'a>>> {
    // Check if window is EVE client
    let character_name = match check_eve_window(ctx, window, state)? {
        Some(name) => name,
        None => return Ok(None),
    };

    // Skip thumbnail creation if thumbnails are disabled (but window is still tracked for hotkeys)
    if !ctx.config.enabled {
        debug!(
            window = window,
            character = %character_name,
            "Skipping thumbnail creation (thumbnails disabled), window still tracked for hotkeys"
        );
        return Ok(None);
    }

    // Get saved position and dimensions for this character/window
    let position = state.get_position(
        &character_name,
        window,
        &daemon_config.character_thumbnails,
        daemon_config.profile.thumbnail_preserve_position_on_swap,
    );

    // Get dimensions from CharacterSettings or use auto-detected defaults
    let dimensions = if let Some(settings) = daemon_config.character_thumbnails.get(&character_name)
    {
        // If dimensions are 0 (not yet saved), auto-detect
        if settings.dimensions.width == 0 || settings.dimensions.height == 0 {
            let (w, h) = daemon_config
                .default_thumbnail_size(ctx.screen.width_in_pixels, ctx.screen.height_in_pixels);
            Dimensions::new(w, h)
        } else {
            settings.dimensions
        }
    } else {
        // Character not in settings yet - auto-detect
        let (w, h) = daemon_config
            .default_thumbnail_size(ctx.screen.width_in_pixels, ctx.screen.height_in_pixels);
        Dimensions::new(w, h)
    };

    let mut thumbnail = Thumbnail::new(
        ctx,
        character_name.clone(),
        window,
        ctx.font_renderer,
        position,
        dimensions,
    )
    .context(format!(
        "Failed to create thumbnail for '{}' (window {})",
        character_name, window
    ))?;
    if is_window_minimized(ctx.conn, window, ctx.atoms).context(format!(
        "Failed to query minimized state for window {}",
        window
    ))? {
        debug!(window = window, character = %character_name, "Window minimized at startup");
        thumbnail.minimized().context(format!(
            "Failed to set minimized state for '{}'",
            character_name
        ))?;
    }
    info!(
        window = window,
        character = %character_name,
        "Created thumbnail for EVE window"
    );
    Ok(Some(thumbnail))
}

/// Identifies if a process is running under Wine/Proton by inspecting its environment and executable path.
/// EVE Online on Linux always runs under Wine, so this distinguishes EVE clients from native Linux processes.
fn is_wine_process(pid: u32) -> bool {
    let pid_str = pid.to_string();

    // 1. Check executable name (readlink /proc/{pid}/exe)
    if let Ok(path) = std::fs::read_link(paths::PROC_EXE_FORMAT.replace("{}", &pid_str)) {
        let path_str = path.to_string_lossy();
        if wine::WINE_PROCESS_NAMES
            .iter()
            .any(|name| path_str.contains(name))
        {
            return true;
        }
        // Also check if it's exefile.exe directly (custom wine builds might expose it)
        if path_str.ends_with(wine::EVE_EXE_NAME) {
            return true;
        }
    } else {
        // If we can't read exe (EPERM), assume it might be Wine if other checks pass
        // or just default to true like the original code (brittle, but safer for now)
        // Original code: inspect_err -> unwrap_or(true)
        // We defer to other checks.
        debug!(
            pid = pid,
            "Cannot read /proc/{pid}/exe, trying other checks"
        );
    }

    // 2. Check command line arguments for EVE executable name
    if let Ok(mut cmdline_file) = std::fs::File::open(format!("/proc/{}/cmdline", pid)) {
        let mut cmdline = String::new();
        // Ignoring errors reading cmdline
        if std::io::Read::read_to_string(&mut cmdline_file, &mut cmdline).is_ok()
            && cmdline.contains(wine::EVE_EXE_NAME)
        {
            return true;
        }
    }

    // 3. Check environment variables for Wine/Proton markers
    // This requires reading /proc/{pid}/environ which are null-delimited strings
    if let Ok(mut environ_file) = std::fs::File::open(format!("/proc/{}/environ", pid)) {
        let mut environ_data = Vec::new();
        if std::io::Read::read_to_end(&mut environ_file, &mut environ_data).is_ok() {
            // Very basic check: search for variable names
            for var in wine::WINE_ENV_VARS {
                // Search for byte sequence "VAR="
                let needle = format!("{}=", var);
                // We can do a string search on the whole block since it's UTF-8ish
                // or just bytes check.
                // Simple approach: efficient byte search
                #[allow(clippy::manual_contains)] // rust versions vary
                if String::from_utf8_lossy(&environ_data).contains(&needle) {
                    return true;
                }
            }
        }
    }

    false
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
        if let Some(eve) = check_and_create_window(ctx, daemon_config, w, state).context(
            format!("Failed to process window {} during initial scan", w),
        )? {
            // Save initial position and dimensions (important for first-time characters)
            // Query geometry to get actual position from X11
            let geom = ctx
                .conn
                .get_geometry(eve.window)
                .context("Failed to query geometry during initial scan")?
                .reply()
                .context("Failed to get geometry reply during initial scan")?;

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

            eve_clients.insert(w, eve);
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
