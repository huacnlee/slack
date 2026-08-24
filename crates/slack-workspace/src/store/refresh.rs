//! Filling the store: from disk first, then from Slack.
//!
//! The cache is read before any network call, so the first frame is already a
//! complete workspace. The refresh that follows replaces it stage by stage
//! rather than all at once, and each stage is written back to disk.

use super::*;

impl WorkspaceStore {
    /// Refresh everything the workspace is built from, in the background.
    ///
    /// The stages run in sequence and each is applied the moment it lands, so
    /// the sidebar appears as soon as the conversation list is known rather
    /// than after the directory and emoji have also been paged in.
    ///
    /// They are deliberately *not* issued concurrently. Slack rate-limits per
    /// method, and firing several paged endpoints at once trips that limit;
    /// the client then backs off for tens of seconds and the whole refresh
    /// takes longer than doing one thing at a time.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.conversations.is_empty() {
            self.load_state = LoadState::Loading;
        }
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            // Stage one: the conversation list, which is the sidebar.
            let conversations = client.list_conversations(ALL_CONVERSATION_TYPES).await;
            let carry_on = this.update(cx, |this, cx| match conversations {
                Ok(channels) => {
                    log::info!("conversation list refreshed: {} rows", channels.len());
                    this.connectivity = Connectivity::Online;
                    this.apply_channels(channels);
                    this.load_state = LoadState::Ready;
                    this.ensure_selection(cx);
                    this.persist();
                    cx.emit(WorkspaceEvent::ConversationsChanged);
                    cx.notify();
                    true
                }
                Err(err) => {
                    if err.is_auth_failure() {
                        cx.emit(WorkspaceEvent::SignedOut);
                        return false;
                    }
                    // A failed refresh with a cache is not a failure the
                    // reader needs to act on; it is offline.
                    this.connectivity = Connectivity::Offline;
                    if this.conversations.is_empty() {
                        this.load_state = LoadState::Failed(err.to_string().into());
                    } else {
                        log::info!("refresh failed, staying on cached workspace: {err}");
                    }
                    cx.emit(WorkspaceEvent::StatusChanged);
                    cx.notify();
                    false
                }
            });
            if !matches!(carry_on, Ok(true)) {
                return;
            }

            // Stage two: the directory, which turns ids into names.
            match client.list_users(DIRECTORY_LIMIT).await {
                Ok(users) => {
                    _ = this.update(cx, |this, cx| {
                        // Merged, not replaced. Members of shared channels are
                        // not in `users.list` and are looked up one at a time;
                        // replacing the map would discard those the moment the
                        // directory finished loading.
                        this.users.extend(
                            users
                                .iter()
                                .map(|u| (SharedString::from(u.id.clone()), u.clone())),
                        );
                        let directory: Vec<User> = this.users.values().cloned().collect();
                        this.cache.write(CACHE_USERS, &directory);
                        this.rename_direct_messages();
                        this.sort_conversations();
                        this.persist();
                        cx.emit(WorkspaceEvent::DirectoryChanged);
                        cx.emit(WorkspaceEvent::ConversationsChanged);
                        cx.notify();
                    });
                }
                Err(err) => log::warn!("users.list failed: {err}"),
            }

            // Stage three: emoji, then the cheap status calls.
            match client.list_custom_emoji().await {
                Ok(emoji) => {
                    _ = this.update(cx, |this, cx| {
                        this.cache.write(CACHE_EMOJI, &emoji);
                        this.emoji = EmojiIndex::new(emoji);
                        cx.emit(WorkspaceEvent::DirectoryChanged);
                        cx.notify();
                    });
                }
                Err(err) => log::warn!("emoji.list failed: {err}"),
            }

            // Slack's stars need `stars:read`; without it the local set stands
            // on its own, which is why a failure here is not reported.
            match client.starred_conversations().await {
                Ok(starred) => {
                    _ = this.update(cx, |this, cx| {
                        this.apply_slack_stars(&starred);
                        this.persist();
                        cx.emit(WorkspaceEvent::ConversationsChanged);
                        cx.notify();
                    });
                }
                Err(err) => log::debug!("stars.list unavailable: {err}"),
            }

            let dnd = client.dnd_info().await;
            let presence = client.presence(None).await;
            _ = this.update(cx, |this, cx| {
                if let Ok(dnd) = dnd {
                    this.dnd = dnd;
                }
                if let Ok(presence) = presence {
                    this.presence = presence;
                }
                cx.emit(WorkspaceEvent::StatusChanged);
                cx.notify();
            });
        })
        .detach();
    }

    // ----------------------------------------------------------- cache

    /// Populate from disk. Runs before any network call, so the first frame
    /// already has a complete workspace.
    pub(super) fn restore_from_cache(&mut self) {
        if let Some(users) = self.cache.read::<Vec<User>>(CACHE_USERS) {
            self.users = users
                .into_iter()
                .map(|u| (SharedString::from(u.id.clone()), u))
                .collect();
        }
        if let Some(emoji) = self.cache.read::<HashMap<String, String>>(CACHE_EMOJI) {
            self.emoji = EmojiIndex::new(emoji);
        }
        if let Some(snapshot) = self.cache.read::<WorkspaceSnapshot>(CACHE_WORKSPACE)
            && let Some((conversations, selected)) = snapshot.restore()
        {
            self.conversations = conversations;
            self.selected = selected;
            self.rename_direct_messages();
            self.sort_conversations();
            self.load_state = LoadState::Ready;
        }
    }

    /// Merge Slack's starred set in. Local stars are kept: a conversation the
    /// reader pinned here should not vanish because Slack does not know it.
    pub(super) fn apply_slack_stars(&mut self, starred: &[String]) {
        for conversation in &mut self.conversations {
            if starred.iter().any(|id| *id == conversation.id) {
                conversation.starred = true;
            }
        }
        self.sort_conversations();
    }

    pub(super) fn persist(&self) {
        self.cache.write(
            CACHE_WORKSPACE,
            &WorkspaceSnapshot::new(&self.conversations, self.selected.as_ref()),
        );
    }

    // ----------------------------------------------------------- internals

    /// Open something once the list is known, so the window is never empty.
    pub(super) fn ensure_selection(&mut self, cx: &mut Context<Self>) {
        let still_present = self
            .selected
            .as_ref()
            .is_some_and(|id| self.conversations.iter().any(|c| c.id == *id));
        if still_present {
            return;
        }
        let first = self.listable().next().map(|c| c.id.clone());
        if let Some(first) = first {
            self.selected = Some(first);
            cx.emit(WorkspaceEvent::SelectionChanged);
        }
    }

    /// Merge a freshly listed set of channels over what is already known.
    ///
    /// Everything derived — unread, latest, probe results — lives only here,
    /// so it must survive a refresh that knows none of it.
    pub(super) fn apply_channels(&mut self, channels: Vec<Channel>) {
        let known: HashMap<SharedString, Conversation> = self
            .conversations
            .drain(..)
            .map(|c| (c.id.clone(), c))
            .collect();

        self.conversations = channels
            .into_iter()
            .map(|channel| {
                let id = SharedString::from(channel.id.clone());
                let previous = known.get(&id);
                Conversation {
                    kind: channel.kind(),
                    name: SharedString::from(channel.name.clone()),
                    topic: channel
                        .topic
                        .as_ref()
                        .map(|t| SharedString::from(t.value.clone()))
                        .unwrap_or_default(),
                    unread: previous.map(|p| p.unread).unwrap_or(0),
                    last_read: channel
                        .last_read
                        .clone()
                        .or_else(|| previous.map(|p| p.last_read.clone()))
                        .unwrap_or_default(),
                    latest: channel
                        .latest
                        .as_ref()
                        .map(|m| m.ts.clone())
                        .or_else(|| previous.and_then(|p| p.latest.clone())),
                    counterpart: channel.user.clone().map(SharedString::from),
                    is_member: channel.is_member || channel.kind().is_dm(),
                    known_empty: previous.is_some_and(|p| p.known_empty),
                    probed_at: previous.map(|p| p.probed_at).unwrap_or(0),
                    starred: previous.is_some_and(|p| p.starred),
                    id,
                }
            })
            .collect();

        self.rename_direct_messages();
        self.sort_conversations();
    }

    /// A DM arrives named only by the other person's id; the directory turns
    /// that into a name once it has loaded.
    pub(super) fn rename_direct_messages(&mut self) {
        for conversation in &mut self.conversations {
            if conversation.kind != ChannelKind::Im {
                continue;
            }
            let Some(counterpart) = conversation.counterpart.clone() else {
                continue;
            };
            conversation.name = match self.users.get(&counterpart) {
                Some(user) => user.display_name().to_string().into(),
                None => counterpart,
            };
        }
    }

    /// Alphabetical, and nothing else.
    ///
    /// The background sweep learns timestamps for minutes after launch, and an
    /// order that depended on them would rearrange the list under the reader's
    /// pointer every few seconds. Recency belongs to the one section where it
    /// helps — direct messages — and the sidebar applies it there.
    pub(super) fn sort_conversations(&mut self) {
        self.conversations.sort_by_key(|c| c.name.to_lowercase());
    }
}
