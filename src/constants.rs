//! Application-wide constants
//!
//! This module contains all magic numbers and string literals used throughout
//! the application, providing a single source of truth for constant values.

/// X11 protocol and rendering constants
pub mod x11 {
    /// Standard 32-bit color depth required for X11 composition
    pub const ARGB_DEPTH: u8 = 32;
    
    /// Size of PID property value in bytes
    pub const PID_PROPERTY_SIZE: usize = 4;
    
    /// Override redirect flag for unmanaged windows
    pub const OVERRIDE_REDIRECT: u32 = 1;
    
    /// Source indication for _NET_ACTIVE_WINDOW (2 = pager/direct user action)
    pub const ACTIVE_WINDOW_SOURCE_PAGER: u32 = 2;
    
    /// _NET_WM_STATE action: add/set property (1)
    pub const NET_WM_STATE_ADD: u32 = 1;

    /// WM_CHANGE_STATE iconic value (requests the WM to minimize)
    pub const ICONIC_STATE: u32 = 3;
}

/// Input event constants (from evdev)
pub mod input {
    /// Key release event value
    pub const KEY_RELEASE: i32 = 0;

    /// Key press event value
    pub const KEY_PRESS: i32 = 1;

    /// Key code for Tab key - used to identify keyboard devices (from Linux input-event-codes.h)
    pub const KEY_TAB: u16 = 15;

    /// Key code for Left Shift key
    pub const KEY_LEFTSHIFT: u16 = 42;

    /// Key code for Right Shift key
    pub const KEY_RIGHTSHIFT: u16 = 54;

    /// Button code for left mouse button - used to identify mouse devices (BTN_LEFT = 0x110)
    pub const BTN_LEFT: u16 = 272;
    /// Button code for right mouse button (BTN_RIGHT = 0x111)
    pub const BTN_RIGHT: u16 = 273;
}

/// Mouse button constants
pub mod mouse {
    /// Left mouse button number
    pub const BUTTON_LEFT: u8 = 1;
    /// Right mouse button number
    pub const BUTTON_RIGHT: u8 = 3;
}

/// Wine process detection constants
pub mod wine {
    /// Common Wine process names to check against
    pub const WINE_PROCESS_NAMES: &[&str] = &[
        "wine64-preloader",
        "wine-preloader",
        "wineserver",
    ];

    /// EVE Online executable name
    pub const EVE_EXE_NAME: &str = "exefile.exe";

    /// Environment variables that indicate a Wine/Proton environment
    pub const WINE_ENV_VARS: &[&str] = &[
        "WINEPREFIX",
        "WINEARCH",
        "WINELOADER",
        "PROTON_PREFIX", // Proton specific
        "STEAM_COMPAT_DATA_PATH", // Proton specific
    ];
}

/// EVE Online window detection constants
pub mod eve {
    /// Prefix for EVE client window titles (followed by character name)
    pub const WINDOW_TITLE_PREFIX: &str = "EVE - ";
    
    /// Window title for logged-out EVE clients
    pub const LOGGED_OUT_TITLE: &str = "EVE";
    
    /// Display name for logged-out character (shown in logs)
    pub const LOGGED_OUT_DISPLAY_NAME: &str = "login_screen";

    /// Known WM_CLASS values for EVE Online
    pub const WINDOW_CLASSES: &[&str] = &[
        "exefile.exe",
        "wine", // Fallback for some wine configs
    ];
}

/// Default window positioning constants
pub mod positioning {
    /// Padding offset from source window when spawning thumbnails
    pub const DEFAULT_SPAWN_OFFSET: i16 = 20;
}

/// Fixed-point arithmetic constants (X11 render transforms)
pub mod fixed_point {
    /// Fixed-point multiplier for conversion (2^16)
    pub const MULTIPLIER: f32 = 65536.0;
}

/// System paths
pub mod paths {
    /// Path format to resolve process executables via /proc/PID/exe
    pub const PROC_EXE_FORMAT: &str = "/proc/{}/exe";
    
    /// Input device directory
    pub const DEV_INPUT: &str = "/dev/input";
}

/// User group permissions
pub mod permissions {
    /// Linux group name for input device access
    pub const INPUT_GROUP: &str = "input";
    
    /// Command to add user to input group
    pub const ADD_TO_INPUT_GROUP: &str = "sudo usermod -a -G input $USER";
}

/// Configuration paths and filenames
pub mod config {
    /// Application directory name under XDG config
    pub const APP_DIR: &str = "eve-preview-manager";
    
    /// Configuration filename
    pub const FILENAME: &str = "config.json";
}

/// GUI-specific constants (egui manager window)
pub mod gui {
    use egui;

    /// Manager window dimensions
    pub const WINDOW_MIN_WIDTH: f32 = 500.0;
    pub const WINDOW_MIN_HEIGHT: f32 = 600.0;
    
    /// Layout spacing
    pub const SECTION_SPACING: f32 = 15.0;
    pub const ITEM_SPACING: f32 = 8.0;
    
    /// Status colors
    pub const STATUS_RUNNING: egui::Color32 = egui::Color32::from_rgb(100, 200, 100);
    pub const STATUS_STARTING: egui::Color32 = egui::Color32::from_rgb(255, 200, 0);
    pub const STATUS_STOPPED: egui::Color32 = egui::Color32::from_rgb(200, 0, 0);
    
    /// Alert level colors
    pub const COLOR_SUCCESS: egui::Color32 = egui::Color32::from_rgb(100, 200, 100);  // Green - success messages
    pub const COLOR_WARNING: egui::Color32 = egui::Color32::from_rgb(255, 200, 0);    // Yellow - warnings/unsaved
    pub const COLOR_ERROR: egui::Color32 = egui::Color32::from_rgb(200, 100, 100);    // Red - errors/discard
    
    /// Daemon monitoring
    pub const DAEMON_CHECK_INTERVAL_MS: u64 = 500;
}

/// Default configuration values
/// These are used when creating new profiles or missing config fields
pub mod defaults {
    /// GUI manager window settings
    pub mod manager {
        /// Default GUI window width in pixels
        pub const WINDOW_WIDTH: u16 = 1020;

        /// Default GUI window height in pixels
        pub const WINDOW_HEIGHT: u16 = 770;
    }
    
    /// Thumbnail window settings
    pub mod thumbnail {
        /// Default thumbnail width in pixels
        pub const WIDTH: u16 = 250;
        
        /// Default thumbnail height in pixels
        pub const HEIGHT: u16 = 140;
        
        /// Default opacity percentage (0-100)
        pub const OPACITY_PERCENT: u8 = 75;
    }
    
    /// Border appearance settings
    pub mod border {
        /// Whether border is enabled by default
        pub const ENABLED: bool = true;
        
        /// Default border thickness in pixels
        pub const SIZE: u16 = 3;
        
        /// Default border color
        pub const COLOR: &str = "#40FF00";
    }
    
    /// Text overlay settings
    pub mod text {
        /// Default text size in pixels
        pub const SIZE: u16 = 22;
        
        /// Default text X offset from left edge in pixels
        pub const OFFSET_X: i16 = 10;
        
        /// Default text Y offset from top edge in pixels
        pub const OFFSET_Y: i16 = 10;
        
        /// Default text color
        pub const COLOR: &str = "#40FF00";
        
        /// Preferred TrueType fonts (tried in order)
        /// First available font will be selected
        pub const FONT_CANDIDATES: &[&str] = &[
            "DejaVu Sans Mono Book",
            "Liberation Mono",
            "Noto Sans Mono",
        ];
    }
    
    /// Daemon behavior settings
    pub mod behavior {
        /// Default profile name
        pub const PROFILE_NAME: &str = "default";
        
        /// Default profile description
        pub const PROFILE_DESCRIPTION: &str = "Default profile";
        
        /// Edge/corner snapping threshold in pixels
        pub const SNAP_THRESHOLD: u16 = 15;
        
        /// Preserve thumbnail position when character switches
        pub const PRESERVE_POSITION_ON_SWAP: bool = true;
        
        /// Minimize other clients when switching via hotkey
        pub const MINIMIZE_CLIENTS_ON_SWITCH: bool = false;
        
        /// Require EVE window focus for hotkey activation
        pub const HOTKEY_REQUIRE_EVE_FOCUS: bool = true;
        
        /// Hide thumbnails when EVE window loses focus
        pub const HIDE_WHEN_NO_FOCUS: bool = false;
    }
}
