use serde::Deserialize;
use serde_json::json;

use crate::client::SlackClient;
use crate::error::Result;
use crate::models::{Channel, Message, MessagePage, Ts};

#[derive(Deserialize)]
struct ListReply {
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    response_metadata: Option<Cursor>,
}

#[derive(Deserialize)]
struct Cursor {
    #[serde(default)]
    next_cursor: String,
}

#[derive(Deserialize)]
struct InfoReply {
    channel: Channel,
}

#[derive(Deserialize)]
struct HistoryReply {
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    response_metadata: Option<Cursor>,
}

#[derive(Deserialize)]
struct MembersReply {
    #[serde(default)]
    members: Vec<String>,
}

/// The conversation kinds the sidebar lists, in Slack's own vocabulary.
pub const ALL_CONVERSATION_TYPES: &str = "public_channel,private_channel,mpim,im";

impl SlackClient {
    /// Every conversation the signed-in user belongs to.
    ///
    /// Slack pages this at 200 rows; the loop follows the cursor so the
    /// sidebar sees a complete workspace rather than a first page.
    pub async fn list_conversations(&self, types: &str) -> Result<Vec<Channel>> {
        let mut all = Vec::new();
        let mut cursor = String::new();

        loop {
            let mut params = vec![
                ("types", types.to_string()),
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
            ];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.clone()));
            }

            let reply: ListReply = self.get("users.conversations", &params).await?;
            all.extend(reply.channels);
            log::debug!("users.conversations page: {} total so far", all.len());

            match reply.response_metadata {
                Some(meta) if !meta.next_cursor.is_empty() => cursor = meta.next_cursor,
                _ => break,
            }
            // A workspace with thousands of conversations should not stall
            // startup; the sidebar can load the rest on demand.
            if all.len() >= 1000 {
                break;
            }
        }

        Ok(all)
    }

    /// Metadata for one conversation, including its unread count.
    pub async fn conversation_info(&self, channel: &str) -> Result<Channel> {
        let reply: InfoReply = self
            .get(
                "conversations.info",
                &[
                    ("channel", channel.to_string()),
                    ("include_num_members", "true".to_string()),
                ],
            )
            .await?;
        Ok(reply.channel)
    }

    /// Channel transcript, returned oldest-first.
    ///
    /// `before` walks backwards for scroll-up paging; leave it `None` for the
    /// newest page.
    pub async fn conversation_history(
        &self,
        channel: &str,
        limit: u32,
        before: Option<&Ts>,
    ) -> Result<MessagePage> {
        let mut params = vec![
            ("channel", channel.to_string()),
            ("limit", limit.to_string()),
            ("include_all_metadata", "true".to_string()),
        ];
        if let Some(ts) = before {
            params.push(("latest", ts.to_string()));
            params.push(("inclusive", "false".to_string()));
        }

        let reply: HistoryReply = self.get("conversations.history", &params).await?;
        Ok(into_page(reply, true))
    }

    /// Messages newer than `after`, used by the poller to pick up new traffic
    /// without refetching the whole transcript.
    pub async fn conversation_history_since(
        &self,
        channel: &str,
        after: &Ts,
        limit: u32,
    ) -> Result<MessagePage> {
        let params = vec![
            ("channel", channel.to_string()),
            ("oldest", after.to_string()),
            ("inclusive", "false".to_string()),
            ("limit", limit.to_string()),
        ];
        let reply: HistoryReply = self.get("conversations.history", &params).await?;
        Ok(into_page(reply, true))
    }

    /// A thread's parent plus its replies, oldest-first.
    pub async fn conversation_replies(
        &self,
        channel: &str,
        thread_ts: &Ts,
        limit: u32,
    ) -> Result<MessagePage> {
        let params = vec![
            ("channel", channel.to_string()),
            ("ts", thread_ts.to_string()),
            ("limit", limit.to_string()),
        ];
        let reply: HistoryReply = self.get("conversations.replies", &params).await?;
        // `conversations.replies` already returns oldest-first.
        Ok(into_page(reply, false))
    }

    /// Move the workspace-wide read marker, so other Slack clients agree with
    /// this one about what has been read.
    pub async fn mark_read(&self, channel: &str, ts: &Ts) -> Result<()> {
        let _: serde_json::Value = self
            .post_json(
                "conversations.mark",
                json!({ "channel": channel, "ts": ts.as_str() }),
            )
            .await?;
        Ok(())
    }

    /// Member ids of a conversation, capped at one page — enough to render a
    /// member count and a few faces.
    pub async fn conversation_members(&self, channel: &str, limit: u32) -> Result<Vec<String>> {
        let reply: MembersReply = self
            .get(
                "conversations.members",
                &[
                    ("channel", channel.to_string()),
                    ("limit", limit.to_string()),
                ],
            )
            .await?;
        Ok(reply.members)
    }

    /// Open (or find) the DM with `user` and return its channel id.
    pub async fn open_dm(&self, user: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct OpenReply {
            channel: OpenChannel,
        }
        #[derive(Deserialize)]
        struct OpenChannel {
            id: String,
        }

        let reply: OpenReply = self
            .post_json("conversations.open", json!({ "users": user }))
            .await?;
        Ok(reply.channel.id)
    }

    /// Create a channel and return it.
    ///
    /// Slack lowercases the name and rejects spaces, so callers should pass a
    /// name that is already in that shape.
    pub async fn create_channel(&self, name: &str, private: bool) -> Result<Channel> {
        let reply: InfoReply = self
            .post_json(
                "conversations.create",
                json!({ "name": name, "is_private": private }),
            )
            .await?;
        Ok(reply.channel)
    }

    pub async fn join_conversation(&self, channel: &str) -> Result<()> {
        let _: serde_json::Value = self
            .post_json("conversations.join", json!({ "channel": channel }))
            .await?;
        Ok(())
    }
}

/// Slack returns history newest-first; the transcript reads oldest-first.
fn into_page(reply: HistoryReply, reverse: bool) -> MessagePage {
    let mut messages = reply.messages;
    if reverse {
        messages.reverse();
    }
    MessagePage {
        messages,
        has_more: reply.has_more,
        next_cursor: reply
            .response_metadata
            .map(|c| c.next_cursor)
            .filter(|c| !c.is_empty()),
    }
}
