use std::collections::HashMap;

use serde::Deserialize;

use crate::client::SlackClient;
use crate::error::Result;

#[derive(Deserialize)]
struct EmojiReply {
    #[serde(default)]
    emoji: HashMap<String, String>,
}

impl SlackClient {
    /// The workspace's custom emoji, as `name -> image URL`.
    ///
    /// Values may be `alias:other-name`; the caller resolves those.
    pub async fn list_custom_emoji(&self) -> Result<HashMap<String, String>> {
        let reply: EmojiReply = self.get("emoji.list", &[]).await?;
        Ok(reply.emoji)
    }
}
