use serde::Deserialize;

use crate::client::SlackClient;
use crate::error::Result;

#[derive(Deserialize)]
struct StarsReply {
    #[serde(default)]
    items: Vec<StarItem>,
}

#[derive(Deserialize)]
struct StarItem {
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    channel: Option<String>,
}

impl SlackClient {
    /// The conversations the user has starred in Slack.
    ///
    /// Needs `stars:read`. Stars on messages and files are ignored: this
    /// client only has a place to show a starred *conversation*.
    pub async fn starred_conversations(&self) -> Result<Vec<String>> {
        let mut starred = Vec::new();
        let reply: StarsReply = self
            .get("stars.list", &[("limit", "200".to_string())])
            .await?;

        for item in reply.items {
            if matches!(item.kind.as_str(), "channel" | "im" | "group" | "mpim")
                && let Some(channel) = item.channel
            {
                starred.push(channel);
            }
        }
        Ok(starred)
    }
}
