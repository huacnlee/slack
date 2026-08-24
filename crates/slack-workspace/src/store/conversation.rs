//! What a conversation is, once the store has made sense of it.
//!
//! These are the shapes the sidebar and the panes read. They carry no
//! behaviour beyond what can be decided from one conversation alone —
//! anything that needs the workspace as a whole lives on the store.

use super::*;

#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: SharedString,
    pub kind: ChannelKind,
    /// Display name without a leading `#`, and resolved to a person for DMs.
    pub name: SharedString,
    pub topic: SharedString,
    /// Messages newer than [`Self::last_read`], as far as the sweep knows.
    pub unread: u32,
    pub last_read: Ts,
    /// Newest message, learned by the sweep. `None` means "not yet probed",
    /// which is different from [`Self::known_empty`].
    pub latest: Option<Ts>,
    /// The other participant, for a one-to-one DM.
    pub counterpart: Option<SharedString>,
    pub is_member: bool,
    /// A probe found no messages at all. Slack keeps every DM ever opened, so
    /// roughly half of them are empty and would otherwise be dead sidebar rows.
    pub known_empty: bool,
    /// Unix seconds of the last metadata probe; 0 means never probed.
    pub probed_at: i64,
    /// Pinned to the top of the sidebar. Seeded from Slack's own stars when
    /// the token carries `stars:read`, and toggled locally otherwise.
    pub starred: bool,
}

impl Conversation {
    pub fn has_unread(&self) -> bool {
        self.unread > 0
    }

    /// Which sidebar section this belongs in.
    pub fn section(&self) -> Section {
        if self.starred {
            Section::Starred
        } else if self.kind.is_dm() {
            Section::DirectMessages
        } else {
            Section::Channels
        }
    }

    /// Whether this belongs in the sidebar.
    ///
    /// An empty direct message is hidden; an empty channel you joined is not,
    /// because you chose to be there and will want to post the first message.
    pub fn is_listable(&self) -> bool {
        !(self.known_empty && self.kind.is_dm())
    }

    /// Ordering weight for the sweep: what the reader is most likely to care
    /// about learning next.
    pub(super) fn probe_priority(&self, now: i64) -> (u8, i64) {
        let never_probed = self.probed_at == 0;
        let tier = match (never_probed, self.kind.is_dm()) {
            (true, true) => 0,
            (true, false) => 1,
            (false, _) => 2,
        };
        // Within a tier, the least recently probed goes first.
        (tier, self.probed_at.saturating_sub(now))
    }
}

/// The sidebar's top-level groups, in the order they are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Starred,
    Channels,
    DirectMessages,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Starred, Section::Channels, Section::DirectMessages];

    pub fn label(self) -> &'static str {
        match self {
            Section::Starred => "Starred",
            Section::Channels => "Channels",
            Section::DirectMessages => "Direct messages",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Section::Starred => "starred",
            Section::Channels => "channels",
            Section::DirectMessages => "dms",
        }
    }
}

/// Whether the store is talking to Slack or serving what it remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    Online,
    /// The last refresh failed; everything on screen came from the cache.
    Offline,
}

/// Where the initial load has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// Nothing to show yet — only on a first run with no cache.
    Loading,
    Ready,
    /// No cache and the network failed, so there is genuinely nothing to show.
    Failed(SharedString),
}

/// Conversations whose name matches `query`, best first.
///
/// Prefix matches rank above substring ones and nothing else is reordered, so
/// a caller that listed alphabetically still reads alphabetically underneath.
/// One matcher for every surface that asks "which conversation?" — the quick
/// switcher and forwarding must not disagree about what a query means.
pub fn matching<'a>(
    conversations: impl Iterator<Item = &'a Conversation>,
    query: &str,
    limit: usize,
) -> Vec<Conversation> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return conversations.take(limit).cloned().collect();
    }

    let mut ranked: Vec<(u8, &Conversation)> = conversations
        .filter_map(|conversation| {
            let name = conversation.name.to_lowercase();
            if name.starts_with(&query) {
                Some((0, conversation))
            } else if name.contains(&query) {
                Some((1, conversation))
            } else {
                None
            }
        })
        .collect();

    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, conversation)| conversation.clone())
        .collect()
}

#[cfg(test)]
pub(in crate::store) mod tests {
    use super::*;

    pub(in crate::store) fn a_conversation(
        id: &str,
        name: &str,
        unread: u32,
        latest: &str,
    ) -> Conversation {
        Conversation {
            id: id.into(),
            kind: ChannelKind::Public,
            name: name.into(),
            topic: SharedString::default(),
            unread,
            last_read: Ts::default(),
            latest: Some(Ts(latest.into())),
            counterpart: None,
            is_member: true,
            known_empty: false,
            probed_at: 0,
            starred: false,
        }
    }

    #[test]
    fn a_prefix_match_outranks_a_substring_one() {
        let general = a_conversation("C1", "general", 0, "0");
        let engineering = a_conversation("C2", "engineering", 0, "0");

        // "general" contains "en"; "engineering" starts with it. Listed the
        // other way round on purpose, so passing means the rank did the work.
        let found = matching([&general, &engineering].into_iter(), "en", 10);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "engineering", "a prefix match should lead");
        assert_eq!(found[1].name, "general");
    }

    #[test]
    fn an_empty_query_keeps_the_order_it_was_given() {
        let a = a_conversation("C1", "zebra", 0, "0");
        let b = a_conversation("C2", "apple", 0, "0");

        let found = matching([&a, &b].into_iter(), "  ", 10);
        assert_eq!(found[0].name, "zebra");
    }

    #[test]
    fn a_query_that_matches_nothing_finds_nothing() {
        let a = a_conversation("C1", "general", 0, "0");
        assert!(matching([&a].into_iter(), "nope", 10).is_empty());
    }

    #[test]
    fn a_starred_conversation_leaves_its_usual_section() {
        let mut dm = a_conversation("D1", "ada", 0, "0");
        dm.kind = ChannelKind::Im;
        assert_eq!(dm.section(), Section::DirectMessages);

        dm.starred = true;
        assert_eq!(dm.section(), Section::Starred);
    }

    #[test]
    fn an_unstarred_channel_stays_in_channels() {
        assert_eq!(
            a_conversation("C1", "general", 0, "0").section(),
            Section::Channels
        );
    }

    #[test]
    fn an_empty_direct_message_is_hidden_but_an_empty_channel_is_not() {
        let mut dm = a_conversation("D1", "ada", 0, "0");
        dm.kind = ChannelKind::Im;
        dm.known_empty = true;
        assert!(!dm.is_listable());

        let mut channel = a_conversation("C1", "general", 0, "0");
        channel.known_empty = true;
        assert!(channel.is_listable());
    }
}
