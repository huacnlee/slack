use serde_json::json;

use crate::client::SlackClient;
use crate::error::Result;
use crate::models::Ts;

impl SlackClient {
    /// Add an emoji reaction. `name` is the short name without colons.
    pub async fn add_reaction(&self, channel: &str, ts: &Ts, name: &str) -> Result<()> {
        let _: serde_json::Value = self
            .post_json(
                "reactions.add",
                json!({ "channel": channel, "timestamp": ts.as_str(), "name": name }),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_reaction(&self, channel: &str, ts: &Ts, name: &str) -> Result<()> {
        let _: serde_json::Value = self
            .post_json(
                "reactions.remove",
                json!({ "channel": channel, "timestamp": ts.as_str(), "name": name }),
            )
            .await?;
        Ok(())
    }
}
