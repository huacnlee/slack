//! Wire shapes for the Slack Web API replies this client reads.
//!
//! Every field Slack may omit is `Option` or defaulted, because the same
//! method returns different shapes for a channel, a group DM, and a bot
//! message. Fields the UI never reads are left out on purpose.

use serde::{Deserialize, Serialize};

/// A message timestamp. Slack uses it as both an ordering key and a message
/// identity, so it is a distinct type rather than a bare `String`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ts(pub String);

impl Ts {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Seconds since the epoch, or 0 when Slack sent something unparseable.
    pub fn epoch_seconds(&self) -> i64 {
        self.0
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// Numeric value used for ordering and read-marker comparison.
    pub fn as_f64(&self) -> f64 {
        self.0.parse().unwrap_or(0.0)
    }
}

impl std::fmt::Display for Ts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Ts {
    fn from(value: String) -> Self {
        Ts(value)
    }
}

/// What kind of conversation a channel row represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Public,
    Private,
    Im,
    Mpim,
}

impl ChannelKind {
    pub fn is_dm(self) -> bool {
        matches!(self, ChannelKind::Im | ChannelKind::Mpim)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub topic: Option<Topic>,
    #[serde(default)]
    pub purpose: Option<Topic>,
    #[serde(default)]
    pub is_channel: bool,
    #[serde(default)]
    pub is_group: bool,
    #[serde(default)]
    pub is_im: bool,
    #[serde(default)]
    pub is_mpim: bool,
    #[serde(default)]
    pub is_private: bool,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub is_member: bool,
    /// Present on DMs: the other participant.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub num_members: Option<u32>,
    #[serde(default)]
    pub unread_count_display: Option<u32>,
    #[serde(default)]
    pub last_read: Option<Ts>,
    #[serde(default)]
    pub latest: Option<Message>,
}

impl Channel {
    pub fn kind(&self) -> ChannelKind {
        if self.is_im {
            ChannelKind::Im
        } else if self.is_mpim {
            ChannelKind::Mpim
        } else if self.is_private {
            ChannelKind::Private
        } else {
            ChannelKind::Public
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Topic {
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub ts: Ts,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub thread_ts: Option<Ts>,
    #[serde(default)]
    pub reply_count: Option<u32>,
    #[serde(default)]
    pub reply_users: Vec<String>,
    #[serde(default)]
    pub latest_reply: Option<Ts>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub files: Vec<File>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub edited: Option<Edited>,
}

impl Message {
    /// A thread parent is a message that has replies hanging off it.
    pub fn is_thread_parent(&self) -> bool {
        self.reply_count.unwrap_or(0) > 0 && self.thread_ts.as_ref().is_none_or(|t| *t == self.ts)
    }

    /// A reply shown inside a thread rather than at channel root.
    pub fn is_thread_reply(&self) -> bool {
        matches!(&self.thread_ts, Some(t) if *t != self.ts)
    }

    /// Join/leave/topic notices render as quiet system lines, not as chat.
    pub fn is_system_notice(&self) -> bool {
        matches!(
            self.subtype.as_deref(),
            Some(
                "channel_join"
                    | "channel_leave"
                    | "group_join"
                    | "group_leave"
                    | "channel_topic"
                    | "channel_purpose"
                    | "channel_name"
                    | "channel_archive"
                    | "channel_unarchive"
                    | "bot_add"
                    | "bot_remove"
            )
        )
    }

    pub fn author_id(&self) -> Option<&str> {
        self.user.as_deref().or(self.bot_id.as_deref())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Edited {
    #[serde(default)]
    pub ts: Ts,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reaction {
    pub name: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct File {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub filetype: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub permalink: Option<String>,
    #[serde(default)]
    pub url_private: Option<String>,
    #[serde(default)]
    pub thumb_360: Option<String>,
    #[serde(default)]
    pub thumb_720: Option<String>,
    /// A still frame, for a video.
    #[serde(default)]
    pub thumb_video: Option<String>,
}

impl File {
    pub fn is_image(&self) -> bool {
        self.mimetype
            .as_deref()
            .is_some_and(|m| m.starts_with("image/"))
    }

    pub fn is_video(&self) -> bool {
        self.mimetype
            .as_deref()
            .is_some_and(|m| m.starts_with("video/"))
    }

    pub fn display_name(&self) -> &str {
        self.title
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or("Untitled file")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub title_link: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub fallback: Option<String>,
    #[serde(default)]
    pub service_name: Option<String>,
    #[serde(default)]
    pub service_icon: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub thumb_url: Option<String>,
    #[serde(default)]
    pub color: Option<String>,

    // A forwarded message arrives as an attachment on an otherwise empty
    // message: everything the reader is meant to see is below.
    /// Set when this attachment *is* another message — a forward, or a Slack
    /// permalink the reader pasted.
    #[serde(default)]
    pub is_share: bool,
    #[serde(default)]
    pub is_msg_unfurl: bool,
    #[serde(default)]
    pub author_name: Option<String>,
    #[serde(default)]
    pub author_subname: Option<String>,
    #[serde(default)]
    pub author_icon: Option<String>,
    #[serde(default)]
    pub author_link: Option<String>,
    #[serde(default)]
    pub author_id: Option<String>,
    #[serde(default)]
    pub channel_name: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    /// When the quoted message was posted.
    ///
    /// Slack sends this as a string for a shared message and as a number for
    /// some link unfurls, so it is read leniently rather than being the one
    /// field that makes a whole message fail to parse.
    #[serde(default, deserialize_with = "lenient_ts")]
    pub ts: Option<Ts>,
    /// A permalink back to the quoted message.
    #[serde(default)]
    pub from_url: Option<String>,
    #[serde(default)]
    pub footer: Option<String>,
    #[serde(default)]
    pub files: Vec<File>,
}

/// Read a timestamp that Slack may have sent as either a string or a number.
fn lenient_ts<'de, D>(deserializer: D) -> std::result::Result<Option<Ts>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        match Option::<serde_json::Value>::deserialize(deserializer)? {
            Some(serde_json::Value::String(s)) if !s.is_empty() => Some(Ts(s)),
            Some(serde_json::Value::Number(n)) => Some(Ts(n.to_string())),
            _ => None,
        },
    )
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub profile: Profile,
}

impl User {
    /// The name to show in a message header or DM row.
    pub fn display_name(&self) -> &str {
        let candidates = [
            self.profile.display_name.as_deref(),
            self.profile.real_name.as_deref(),
            self.real_name.as_deref(),
            Some(self.name.as_str()),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .unwrap_or(&self.id)
    }

    /// The avatar to draw, or `None` when there is nothing renderable.
    ///
    /// WebP is skipped: GPUI's image decoder does not handle it, and a handful
    /// of Slack profiles use it. Passing one through produces a broken image
    /// and a decode error per frame, where an initial reads fine.
    pub fn avatar_url(&self) -> Option<&str> {
        [
            self.profile.image_72.as_deref(),
            self.profile.image_48.as_deref(),
            self.profile.image_192.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|url| !url.is_empty() && !is_webp(url))
    }
}

/// GPUI cannot decode WebP; a URL ending in it is not worth requesting.
fn is_webp(url: &str) -> bool {
    url.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("webp"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status_text: Option<String>,
    #[serde(default)]
    pub status_emoji: Option<String>,
    #[serde(default)]
    pub image_48: Option<String>,
    #[serde(default)]
    pub image_72: Option<String>,
    #[serde(default)]
    pub image_192: Option<String>,
}

/// The reply from `auth.test`: who we are and where.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthIdentity {
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub team_id: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Active,
    Away,
}

impl Presence {
    pub fn as_api_str(self) -> &'static str {
        match self {
            Presence::Active => "auto",
            Presence::Away => "away",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DndState {
    #[serde(default)]
    pub snooze_enabled: bool,
    #[serde(default)]
    pub snooze_endtime: i64,
    #[serde(default)]
    pub dnd_enabled: bool,
}

/// One page of `conversations.history` or `conversations.replies`.
#[derive(Debug, Clone, Default)]
pub struct MessagePage {
    /// Oldest first, which is the order the transcript renders in.
    pub messages: Vec<Message>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_avatars(image_72: &str, image_48: &str) -> User {
        User {
            id: "U1".into(),
            profile: Profile {
                image_72: Some(image_72.to_string()),
                image_48: Some(image_48.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_largest_available_avatar_is_used() {
        let user = with_avatars("https://x/a_72.png", "https://x/a_48.png");
        assert_eq!(user.avatar_url(), Some("https://x/a_72.png"));
    }

    #[test]
    fn a_webp_avatar_falls_through_to_a_format_that_renders() {
        let user = with_avatars("https://x/a_72.webp", "https://x/a_48.png");
        assert_eq!(user.avatar_url(), Some("https://x/a_48.png"));
    }

    #[test]
    fn a_profile_with_only_webp_has_no_avatar() {
        let user = with_avatars("https://x/a_72.webp", "https://x/a_48.WEBP");
        assert_eq!(user.avatar_url(), None);
    }

    #[test]
    fn an_empty_avatar_field_is_not_a_url() {
        let user = with_avatars("", "https://x/a_48.png");
        assert_eq!(user.avatar_url(), Some("https://x/a_48.png"));
    }

    #[test]
    fn a_url_with_no_extension_is_kept() {
        let user = with_avatars("https://x/avatar", "");
        assert_eq!(user.avatar_url(), Some("https://x/avatar"));
    }
}
