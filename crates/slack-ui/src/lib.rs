//! Foundations every feature builds on.
//!
//! Nothing here knows about channels, people, or the workspace: it is the
//! layer a feature crate may depend on freely without coupling itself to a
//! sibling. Keeping it that way is what lets the features be read one at a
//! time.

pub mod actions;
pub mod icons;
pub mod images;
pub mod manifest;
pub mod notify;
pub mod theme;
pub mod time;
