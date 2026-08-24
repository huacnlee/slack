//! One module per Slack API namespace, each an `impl SlackClient` block.
//!
//! Keeping them separate mirrors Slack's own method naming, so the place to
//! look for `conversations.replies` is `conversations.rs`.

mod auth;
mod chat;
mod conversations;
mod dnd;
mod emoji;
mod files;
mod reactions;
mod search;
mod stars;
mod users;

pub use chat::MAX_MESSAGE_CHARS;
pub use conversations::ALL_CONVERSATION_TYPES;
pub use files::MAX_UPLOAD_BYTES;
pub use search::{SearchMatch, SearchResults};
