//! Icons this product needs that the shared set does not ship.
//!
//! All Lucide, the same family `gpui-component-assets` uses, so the two sets
//! cannot drift into looking like two products. The files are taken unmodified
//! from `lucide-static`; see `crates/slack-app/assets/ICONS.md`.
//!
//! `IconNamed` is the documented seam for exactly this: the enum drops into
//! anywhere `IconName` is accepted, and the paths resolve against the
//! application's own asset source.

use gpui::SharedString;
use gpui_component::IconNamed;

/// Product icons served from `assets/icons` in the application binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackIcon {
    /// A public channel.
    Hash,
    /// A private channel.
    Lock,
    /// A group direct message.
    Users,
    /// Mentions and keywords.
    AtSign,
    /// A message thread.
    Thread,
    /// Conversations, for the navigation rail.
    Chats,
    /// Direct messages, for the navigation rail.
    DirectMessages,
    /// Add a reaction.
    SmilePlus,
    /// Attach a file.
    Paperclip,
    Send,
    SignOut,
    /// Notifications paused.
    BellOff,
    Pencil,
    Trash,
    Link,
    Refresh,
    Download,
    FileText,
    /// Leads a threaded reply.
    CornerDownRight,
}

impl IconNamed for SlackIcon {
    fn path(self) -> SharedString {
        let name = match self {
            SlackIcon::Hash => "hash",
            SlackIcon::Lock => "lock",
            SlackIcon::Users => "users",
            SlackIcon::AtSign => "at-sign",
            SlackIcon::Thread => "message-square-reply",
            SlackIcon::Chats => "message-circle",
            SlackIcon::DirectMessages => "messages-square",
            SlackIcon::SmilePlus => "smile-plus",
            SlackIcon::Paperclip => "paperclip",
            SlackIcon::Send => "send",
            SlackIcon::SignOut => "log-out",
            SlackIcon::BellOff => "bell-off",
            SlackIcon::Pencil => "pencil",
            SlackIcon::Trash => "trash-2",
            SlackIcon::Link => "link",
            SlackIcon::Refresh => "refresh-cw",
            SlackIcon::Download => "download",
            SlackIcon::FileText => "file-text",
            SlackIcon::CornerDownRight => "corner-down-right",
        };
        format!("icons/{name}.svg").into()
    }
}

impl SlackIcon {
    /// The icon that leads a conversation row for `kind`.
    pub fn for_channel(kind: slack_api::models::ChannelKind) -> Self {
        use slack_api::models::ChannelKind::*;
        match kind {
            Public => SlackIcon::Hash,
            Private => SlackIcon::Lock,
            Mpim => SlackIcon::Users,
            // A one-to-one DM shows the person's avatar instead of an icon;
            // this is only the fallback while the user is still unknown.
            Im => SlackIcon::AtSign,
        }
    }
}
