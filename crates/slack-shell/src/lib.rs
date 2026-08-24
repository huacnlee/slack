//! The window itself: the rail, the sidebar, and the panel the features are
//! shown in.
//!
//! This crate composes; it should hold as little of a feature as it can get
//! away with. When something here starts to describe how a feature behaves
//! rather than where it sits, it belongs in that feature's crate instead.

pub mod app;
pub mod direct_messages;
pub mod history;
pub mod quick_switcher;
pub mod rail;
pub mod sidebar;
pub mod workspace_view;
