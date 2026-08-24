//! What the workspace currently is: its conversations, members, and unread
//! state, plus the on-disk copy that lets the window open before the network
//! answers.
//!
//! This is the one place that talks to Slack about workspace shape, so a
//! feature can ask what exists without each one growing its own idea of it.

pub mod snapshot;
pub mod store;
