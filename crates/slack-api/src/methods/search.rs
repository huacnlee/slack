use serde::Deserialize;

use crate::client::SlackClient;
use crate::error::Result;
use crate::models::Ts;

/// One page of `search.messages`.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    pub total: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchMatch {
    #[serde(default)]
    pub ts: Ts,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub permalink: Option<String>,
    #[serde(default)]
    pub channel: Option<SearchChannel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchChannel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Deserialize)]
struct SearchReply {
    #[serde(default)]
    messages: Messages,
}

#[derive(Default, Deserialize)]
struct Messages {
    #[serde(default)]
    matches: Vec<SearchMatch>,
    #[serde(default)]
    total: u32,
}

impl SlackClient {
    /// Full-text search across the workspace. Requires a user token with
    /// `search:read`; a bot token will be refused by Slack.
    pub async fn search_messages(&self, query: &str, count: u32) -> Result<SearchResults> {
        let reply: SearchReply = self
            .get(
                "search.messages",
                &[
                    ("query", query.to_string()),
                    ("count", count.to_string()),
                    ("sort", "timestamp".to_string()),
                    ("sort_dir", "desc".to_string()),
                ],
            )
            .await?;
        Ok(SearchResults {
            matches: reply.messages.matches,
            total: reply.messages.total,
        })
    }
}
