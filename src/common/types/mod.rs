//! Domain types for type safety and clarity
//!
//! Refactored into sub-modules for better organization.

pub mod character;
pub mod geometry;

// Re-export specific types to maintain compatibility
pub use character::{CharacterSettings, EveWindowType, PreviewMode, ThumbnailState};
pub use geometry::{Dimensions, Position, TextOffset};
