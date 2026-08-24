use serde::Deserialize;
use serde_json::json;

use crate::client::SlackClient;
use crate::error::Result;
use crate::models::{Presence, User};

#[derive(Deserialize)]
struct UserReply {
    user: User,
}

#[derive(Deserialize)]
struct UserListReply {
    #[serde(default)]
    members: Vec<User>,
    #[serde(default)]
    response_metadata: Option<Cursor>,
}

#[derive(Deserialize)]
struct Cursor {
    #[serde(default)]
    next_cursor: String,
}

#[derive(Deserialize)]
struct PresenceReply {
    #[serde(default)]
    presence: String,
}

impl SlackClient {
    pub async fn user_info(&self, user: &str) -> Result<User> {
        let reply: UserReply = self
            .get("users.info", &[("user", user.to_string())])
            .await?;
        Ok(reply.user)
    }

    /// The workspace directory, paged until `max` users have been collected.
    ///
    /// The client fetches this once at sign-in so message headers and mentions
    /// can resolve names without a lookup per row.
    pub async fn list_users(&self, max: usize) -> Result<Vec<User>> {
        let mut all = Vec::new();
        let mut cursor = String::new();

        loop {
            let mut params = vec![("limit", "200".to_string())];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.clone()));
            }

            let reply: UserListReply = self.get("users.list", &params).await?;
            all.extend(reply.members);

            match reply.response_metadata {
                Some(meta) if !meta.next_cursor.is_empty() && all.len() < max => {
                    cursor = meta.next_cursor
                }
                _ => break,
            }
        }

        all.truncate(max);
        Ok(all)
    }

    pub async fn presence(&self, user: Option<&str>) -> Result<Presence> {
        let params: Vec<(&str, String)> = match user {
            Some(id) => vec![("user", id.to_string())],
            None => vec![],
        };
        let reply: PresenceReply = self.get("users.getPresence", &params).await?;
        Ok(match reply.presence.as_str() {
            "active" => Presence::Active,
            _ => Presence::Away,
        })
    }

    pub async fn set_presence(&self, presence: Presence) -> Result<()> {
        let _: serde_json::Value = self
            .post_form(
                "users.setPresence",
                &[("presence", presence.as_api_str().to_string())],
            )
            .await?;
        Ok(())
    }

    /// Set (or with empty values, clear) the custom status.
    pub async fn set_status(&self, text: &str, emoji: &str, expires_at: i64) -> Result<()> {
        let profile = json!({
            "status_text": text,
            "status_emoji": emoji,
            "status_expiration": expires_at,
        });
        let _: serde_json::Value = self
            .post_json("users.profile.set", json!({ "profile": profile }))
            .await?;
        Ok(())
    }
}
