//! A Slack Web API client for a desktop client.
//!
//! The crate is deliberately free of UI: it owns the transport, the wire
//! shapes, mrkdwn parsing, emoji resolution, and where the token is stored. A
//! view layer composes those; nothing here knows a window exists.
//!
//! ```no_run
//! # async fn run() -> slack_api::Result<()> {
//! let client = slack_api::SlackClient::new("xoxp-…")?;
//! let me = client.auth_test().await?;
//! let channels = client
//!     .list_conversations(slack_api::ALL_CONVERSATION_TYPES)
//!     .await?;
//! println!("{} has {} conversations", me.team, channels.len());
//! # Ok(())
//! # }
//! ```

mod client;
mod error;
mod methods;

pub mod cache;
pub mod dotenv;
pub mod emoji;
pub mod markup;
pub mod models;
pub mod rtm;
pub mod store;

pub use cache::{Cache, Freshness};
pub use client::SlackClient;
pub use error::{Error, Result};
pub use methods::{
    ALL_CONVERSATION_TYPES, MAX_MESSAGE_CHARS, MAX_UPLOAD_BYTES, SearchMatch, SearchResults,
};
pub use rtm::RtmEvent;
