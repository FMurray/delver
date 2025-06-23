//! Debug viewer for visualizing PDF parsing, layout, and template matching

#[cfg(feature = "debug-viewer")]
mod app;
#[cfg(feature = "debug-viewer")]
mod async_app;
#[cfg(feature = "debug-viewer")]
mod event_panel;
#[cfg(feature = "debug-viewer")]
mod match_panel;
#[cfg(feature = "debug-viewer")]
mod rendering;
#[cfg(feature = "debug-viewer")]
mod ui_controls;
#[cfg(feature = "debug-viewer")]
mod utils;

// Re-export the main types and functions
#[cfg(feature = "debug-viewer")]
pub use app::{launch_viewer, DebugViewer};

// Re-export the async viewer
#[cfg(feature = "debug-viewer")]
pub use async_app::{launch_async_viewer, AsyncDebugViewer};

// Re-export panel functions needed for integration
#[cfg(feature = "debug-viewer")]
pub use event_panel::show_event_panel;
#[cfg(feature = "debug-viewer")]
pub use match_panel::show_match_panel;
