//! Configuration management for EVE-Preview-Manager
//!
//! ## Architecture Overview
//!
//! This module manages application configuration with a unified JSON-based system
//! supporting multiple visual profiles and global daemon behavior settings.
//!
//! ### Config Flow
//!
//! ```text
//! JSON File (~/.config/eve-preview-manager/config.json)
//!     ├── global: GlobalSettings (daemon behavior + GUI window state)
//!     │   ├── selected_profile (which profile is active)
//!     │   ├── window_width, window_height (GUI manager window)
//!     │   ├── hide_when_no_focus
//!     │   ├── snap_threshold
//!     │   ├── hotkey_require_eve_focus
//!     │   ├── minimize_clients_on_switch
//!     │   ├── preserve_thumbnail_position_on_swap
//!     │   └── default_thumbnail_width, default_thumbnail_height
//!     └── profiles: Vec<Profile> (visual appearance per profile)
//!         ├── name, description
//!         ├── opacity_percent, border_size, border_color
//!         ├── text_size, text_x, text_y, text_color
//!         ├── cycle_group (hotkey Tab/Shift+Tab order)
//!         └── character_positions (x, y, width, height per character)
//! ```
//!
//! ### Two Config Systems (Different Purposes)
//!
//! #### 1. `profile::Config` - GUI Manager
//! - **Used by**: GUI manager application
//! - **Purpose**: Full configuration with profile management
//! - **Operations**: Load, save, create/edit/delete profiles
//! - **Save strategy**: Preserves character_positions (daemon owns this data)
//!
//! #### 2. `daemon_state::PersistentState` - Daemon Runtime
//! - **Used by**: X11 preview daemon
//! - **Purpose**: Runtime state extracted from selected profile
//! - **Structure**:
//!   ```rust
//!   pub struct PersistentState {
//!       profile: Profile,           // Visual settings from selected profile
//!       global: GlobalSettings,     // Behavior settings (applies to all profiles)
//!       character_positions: HashMap<String, CharacterSettings>,  // Runtime state
//!   }
//!   ```
//! - **Operations**: Load on startup, save character positions during runtime
//! - **Conversion**: `Config::from_profile_config()` extracts selected profile + global settings
//!
//! ### Data Flow
//!
//! **GUI Manager**:
//! ```text
//! 1. Load: Config::load() → Full config with all profiles
//! 2. Edit: User modifies profile visual settings or global behavior
//! 3. Save: Config::save() → Preserves character_positions from disk
//! ```
//!
//! **Preview Daemon**:
//! ```text
//! 1. Load: PersistentState::load() → Extracts selected profile + global
//! 2. Runtime: User drags thumbnails, character positions update
//! 3. Save: PersistentState::save() → Overwrites character_positions in JSON
//! ```
//!
//! ### Clean Separation of Concerns
//!
//! **Visual Settings** (per-profile, in `Profile`):
//! - opacity_percent, border_size, border_color
//! - text_size, text_x, text_y, text_foreground
//! - cycle_group (hotkey order for this profile)
//! - character_positions (window positions/dimensions)
//!
//! **Behavior Settings** (global, in `GlobalSettings`):
//! - selected_profile (which profile is active)
//! - window_width, window_height (GUI manager window dimensions)
//! - hide_when_no_focus (show/hide thumbnails)
//! - snap_threshold (edge snapping distance)
//! - hotkey_require_eve_focus (restrict hotkeys to EVE focus)
//! - minimize_clients_on_switch (minimize other clients on focus)
//! - preserve_thumbnail_position_on_swap (keep position on character change)
//! - default_thumbnail_width, default_thumbnail_height (new thumbnail defaults)
//!
//! ### No Conversion, Just Extraction
//!
//! The daemon doesn't convert or translate config formats - it simply clones
//! the selected Profile and GlobalSettings directly from the JSON config:
//!
//! ```rust
//! fn from_profile_config(config: Config) -> Self {
//!     let profile = find_selected_profile(&config);
//!     Self {
//!         profile: profile.clone(),
//!         global: config.global.clone(),
//!         character_positions: profile.character_positions.clone(),
//!     }
//! }
//! ```
//!
//! This ensures one source of truth with no synchronization issues.

pub mod daemon_state;
pub mod hotkey_binding;
pub mod profile;

// Re-export commonly used types
pub use daemon_state::{DisplayConfig, PersistentState};
pub use hotkey_binding::HotkeyBinding;
