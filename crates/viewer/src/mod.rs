//! Debug viewer for visualizing PDF parsing, layout, and template matching

#[cfg(feature = "viewer")]
pub mod app;
#[cfg(feature = "viewer")]
mod event_panel;
#[cfg(feature = "viewer")]
mod match_panel;
#[cfg(feature = "viewer")]
mod rendering;
#[cfg(feature = "viewer")]
mod ui_controls;
#[cfg(feature = "viewer")]
mod utils;

// Re-export the main types and functions
#[cfg(feature = "viewer")]
pub use app::{Viewer, launch_viewer};

// Re-export panel functions needed for integration
#[cfg(feature = "viewer")]
pub use event_panel::show_event_panel;
#[cfg(feature = "viewer")]
pub use match_panel::show_match_panel;
