//! What the workspace remembers between launches.
//!
//! The persisted shape is deliberately its own type rather than the runtime
//! [`Conversation`]: the cache is a file format that has to survive version
//! changes, and tying it to a struct that carries `SharedString` and view
//! concerns would make every UI refactor a cache migration.
//!
//! `version` is checked on read. A snapshot from an older layout is discarded
//! rather than coerced, because a wrong unread count is worse than none.

use gpui::SharedString;
use serde::{Deserialize, Serialize};

use slack_api::models::{ChannelKind, Ts};

use crate::workspace::store::Conversation;

/// Bumped whenever the meaning of a field changes.
pub const SNAPSHOT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub conversations: Vec<StoredConversation>,
    /// The conversation that was open, so a relaunch lands where you left off.
    #[serde(default)]
    pub selected: Option<String>,
}

impl WorkspaceSnapshot {
    pub fn new(conversations: &[Conversation], selected: Option<&SharedString>) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            conversations: conversations.iter().map(StoredConversation::from).collect(),
            selected: selected.map(|s| s.to_string()),
        }
    }

    /// Read a snapshot back, or `None` if it was written by another version.
    pub fn restore(self) -> Option<(Vec<Conversation>, Option<SharedString>)> {
        if self.version != SNAPSHOT_VERSION {
            log::info!(
                "discarding a workspace snapshot from version {}",
                self.version
            );
            return None;
        }
        Some((
            self.conversations
                .into_iter()
                .map(Conversation::from)
                .collect(),
            self.selected.map(SharedString::from),
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredConversation {
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub counterpart: Option<String>,
    #[serde(default)]
    pub is_member: bool,
    #[serde(default)]
    pub last_read: Ts,
    #[serde(default)]
    pub latest: Option<Ts>,
    #[serde(default)]
    pub unread: u32,
    /// A history probe found no messages at all.
    #[serde(default)]
    pub known_empty: bool,
    /// Unix seconds of the last metadata probe; 0 means never probed.
    #[serde(default)]
    pub probed_at: i64,
    /// Pinned to the top of the sidebar.
    #[serde(default)]
    pub starred: bool,
}

impl From<&Conversation> for StoredConversation {
    fn from(conversation: &Conversation) -> Self {
        Self {
            id: conversation.id.to_string(),
            kind: conversation.kind,
            name: conversation.name.to_string(),
            topic: conversation.topic.to_string(),
            counterpart: conversation.counterpart.as_ref().map(|c| c.to_string()),
            is_member: conversation.is_member,
            last_read: conversation.last_read.clone(),
            latest: conversation.latest.clone(),
            unread: conversation.unread,
            known_empty: conversation.known_empty,
            probed_at: conversation.probed_at,
            starred: conversation.starred,
        }
    }
}

impl From<StoredConversation> for Conversation {
    fn from(stored: StoredConversation) -> Self {
        Self {
            id: stored.id.into(),
            kind: stored.kind,
            name: stored.name.into(),
            topic: stored.topic.into(),
            counterpart: stored.counterpart.map(SharedString::from),
            is_member: stored.is_member,
            last_read: stored.last_read,
            latest: stored.latest,
            unread: stored.unread,
            known_empty: stored.known_empty,
            probed_at: stored.probed_at,
            starred: stored.starred,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Conversation {
        Conversation {
            id: "C0123456".into(),
            kind: ChannelKind::Public,
            name: "general".into(),
            topic: "everything".into(),
            counterpart: None,
            is_member: true,
            last_read: Ts("1700000000.000100".into()),
            latest: Some(Ts("1700000900.000100".into())),
            unread: 4,
            known_empty: false,
            probed_at: 1_700_000_999,
            starred: true,
        }
    }

    #[test]
    fn a_snapshot_round_trips_every_field() {
        let original = conversation();
        let snapshot = WorkspaceSnapshot::new(std::slice::from_ref(&original), None);
        let json = serde_json::to_vec(&snapshot).unwrap();
        let read: WorkspaceSnapshot = serde_json::from_slice(&json).unwrap();

        let (restored, _) = read.restore().expect("same version");
        let restored = &restored[0];

        assert_eq!(restored.id, original.id);
        assert_eq!(restored.kind, original.kind);
        assert_eq!(restored.name, original.name);
        assert_eq!(restored.unread, original.unread);
        assert_eq!(restored.last_read, original.last_read);
        assert_eq!(restored.latest, original.latest);
        assert_eq!(restored.probed_at, original.probed_at);
        assert_eq!(restored.starred, original.starred);
    }

    #[test]
    fn the_open_conversation_is_remembered() {
        let snapshot = WorkspaceSnapshot::new(&[conversation()], Some(&"C0123456".into()));
        let (_, selected) = snapshot.restore().unwrap();
        assert_eq!(selected, Some(SharedString::from("C0123456")));
    }

    #[test]
    fn a_snapshot_from_another_version_is_refused() {
        let mut snapshot = WorkspaceSnapshot::new(&[conversation()], None);
        snapshot.version = SNAPSHOT_VERSION + 1;
        assert!(snapshot.restore().is_none());
    }

    #[test]
    fn missing_optional_fields_read_as_defaults() {
        // What a snapshot written before a field existed looks like.
        let json = r#"{"version":2,"conversations":[
            {"id":"C1","kind":"public","name":"general"}
        ]}"#;
        let snapshot: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        let (conversations, selected) = snapshot.restore().unwrap();

        assert_eq!(conversations[0].unread, 0);
        assert!(conversations[0].latest.is_none());
        assert_eq!(conversations[0].probed_at, 0);
        assert!(!conversations[0].starred);
        assert_eq!(selected, None);
    }
}
