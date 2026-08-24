use serde::Deserialize;
use serde_json::json;

use crate::client::SlackClient;
use crate::error::{Error, Result};
use crate::models::{Message, Ts};

/// Slack's own limit for a single message body.
pub const MAX_MESSAGE_CHARS: usize = 4000;

#[derive(Deserialize)]
struct PostReply {
    #[serde(default)]
    ts: Ts,
}

impl SlackClient {
    /// Post to a channel, or into a thread when `thread_ts` is set.
    ///
    /// Returns the new message's timestamp so the caller can reconcile the
    /// optimistic row it already rendered.
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&Ts>,
    ) -> Result<Ts> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Other("the message is empty".into()));
        }
        if text.chars().count() > MAX_MESSAGE_CHARS {
            return Err(Error::Slack("msg_too_long".into()));
        }

        let mut body = json!({
            "channel": channel,
            "text": text,
            // Slack renders `text` as mrkdwn only when asked to.
            "mrkdwn": true,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts.as_str());
        }

        let reply: PostReply = self.post_json("chat.postMessage", body).await?;
        Ok(reply.ts)
    }

    /// Edit a message the signed-in user owns, and report it back as Slack now
    /// holds it.
    ///
    /// The reply carries the stored message, not an acknowledgement. Returning
    /// it saves the caller refetching a page of history to discover what its
    /// own edit did — and what came back is authoritative where an optimistic
    /// copy would only be a guess.
    pub async fn update_message(&self, channel: &str, ts: &Ts, text: &str) -> Result<Message> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Other("the message is empty".into()));
        }
        if text.chars().count() > MAX_MESSAGE_CHARS {
            return Err(Error::Slack("msg_too_long".into()));
        }

        #[derive(Deserialize)]
        struct UpdateReply {
            #[serde(default)]
            ts: Ts,
            #[serde(default)]
            text: String,
            #[serde(default)]
            message: Option<Message>,
        }

        let reply: UpdateReply = self
            .post_json(
                "chat.update",
                json!({
                    "channel": channel,
                    "ts": ts.as_str(),
                    "text": text,
                    // Matches how the message was posted; without it an edit
                    // would quietly strip the formatting the original had.
                    "mrkdwn": true,
                }),
            )
            .await?;

        // `message` is the whole stored message and is what we want. Slack has
        // been known to answer with only the top-level echo, so fall back to
        // that rather than returning something blank.
        let mut message = reply.message.unwrap_or_default();
        if message.ts.as_str().is_empty() {
            message.ts = reply.ts;
        }
        if message.text.is_empty() {
            message.text = reply.text;
        }
        Ok(message)
    }

    pub async fn delete_message(&self, channel: &str, ts: &Ts) -> Result<()> {
        let _: serde_json::Value = self
            .post_json(
                "chat.delete",
                json!({ "channel": channel, "ts": ts.as_str() }),
            )
            .await?;
        Ok(())
    }

    /// A permalink for one message, used by "Copy link".
    pub async fn message_permalink(&self, channel: &str, ts: &Ts) -> Result<String> {
        #[derive(Deserialize)]
        struct LinkReply {
            #[serde(default)]
            permalink: String,
        }

        let reply: LinkReply = self
            .get(
                "chat.getPermalink",
                &[
                    ("channel", channel.to_string()),
                    ("message_ts", ts.to_string()),
                ],
            )
            .await?;
        Ok(reply.permalink)
    }
}
