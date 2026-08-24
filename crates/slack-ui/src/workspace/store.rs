//! The signed-in workspace: what the whole application reads from.
//!
//! One entity owns the client, the conversation list, the directory, the
//! emoji index, and the presence/DND state. Views observe it rather than
//! holding their own copies, so a refresh from any source updates every
//! surface at once.
//!
//! **Cache first.** Enumerating this workspace over the network takes tens of
//! seconds — a thousand conversations and a thousand members is a dozen paged
//! requests — so the store opens from disk and shows a complete, sorted,
//! badged sidebar on the first frame. The network refresh then runs behind it
//! and writes through. When the network is unreachable the cached workspace is
//! simply what the application is, which is also what makes it work offline.
//!
//! **Unread is derived, not fetched.** Slack returns no unread count and no
//! latest-message timestamp to an OAuth user token: `conversations.info` gives
//! only `last_read`, and the bulk `users.counts` endpoint refuses this token
//! type. So the store learns the newest timestamp per conversation from a
//! budgeted background sweep, compares it against `last_read`, and remembers
//! both. That is why the sweep exists and why its results are persisted.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{AppContext as _, Context, EventEmitter, SharedString, Task};

use slack_api::emoji::EmojiIndex;
use slack_api::models::{AuthIdentity, Channel, ChannelKind, DndState, Presence, Ts, User};
use slack_api::{ALL_CONVERSATION_TYPES, Cache, SlackClient};

use crate::workspace::snapshot::WorkspaceSnapshot;

/// How often the active conversation is checked for new messages.
///
/// Slack apps created after May 2025 are limited to one `conversations.history`
/// request per minute. Older apps get a far higher tier, so the client starts
/// responsive and falls back to [`ACTIVE_POLL_THROTTLED`] the first time Slack
/// says it is asking too often — rather than assuming the worst for everyone.
const ACTIVE_POLL: Duration = Duration::from_secs(6);
const ACTIVE_POLL_THROTTLED: Duration = Duration::from_secs(65);

/// How often the background sweep learns more about the conversation list.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
/// Conversations probed per sweep cycle. Each costs one or two API calls, so
/// this is the knob that keeps a large workspace inside Slack's rate limit
/// while still converging in a few minutes.
const SWEEP_BATCH: usize = 8;
/// How long a probe stays trusted before the sweep refreshes it.
const PROBE_TTL_SECONDS: i64 = 15 * 60;

/// The directory is fetched once; past this many members, mention completion
/// and name resolution fall back to on-demand lookups.
const DIRECTORY_LIMIT: usize = 3000;

/// Cache keys, all scoped to the signed-in team.
const CACHE_WORKSPACE: &str = "workspace";
const CACHE_USERS: &str = "users";
const CACHE_EMOJI: &str = "emoji";

/// A conversation as the sidebar needs it: already named, already counted.
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
    fn probe_priority(&self, now: i64) -> (u8, i64) {
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

#[derive(Debug, Clone)]
pub enum WorkspaceEvent {
    /// The conversation list, its order, or its unread counts changed.
    ConversationsChanged,
    /// A different conversation is now active.
    SelectionChanged,
    /// The directory or emoji index gained entries.
    DirectoryChanged,
    /// The active conversation may have new messages worth fetching.
    ActivityPolled,
    /// Presence, DND, or the identity header changed.
    StatusChanged,
    /// Something the user should be told about.
    Failed(SharedString),
    /// The token stopped working; the shell should return to sign-in.
    SignedOut,
}

pub struct WorkspaceStore {
    client: SlackClient,
    identity: AuthIdentity,
    cache: Cache,
    conversations: Vec<Conversation>,
    users: HashMap<SharedString, User>,
    emoji: EmojiIndex,
    presence: Presence,
    dnd: DndState,
    selected: Option<SharedString>,
    /// Unsent composer text, kept per conversation so switching does not
    /// discard what someone was in the middle of writing.
    drafts: HashMap<SharedString, String>,
    load_state: LoadState,
    connectivity: Connectivity,
    /// How often to look for new messages, widened once Slack rate-limits us.
    activity_interval: Duration,
    _polling: Vec<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for WorkspaceStore {}

impl WorkspaceStore {
    pub fn new(client: SlackClient, identity: AuthIdentity, cx: &mut Context<Self>) -> Self {
        let cache = Cache::for_team(&identity.team_id);

        let mut store = Self {
            client,
            identity,
            cache,
            conversations: Vec::new(),
            users: HashMap::new(),
            emoji: EmojiIndex::default(),
            presence: Presence::Active,
            dnd: DndState::default(),
            selected: None,
            drafts: HashMap::new(),
            load_state: LoadState::Loading,
            connectivity: Connectivity::Online,
            activity_interval: ACTIVE_POLL,
            _polling: Vec::new(),
        };

        let started = std::time::Instant::now();
        store.restore_from_cache();
        log::info!(
            "workspace opened from cache in {:?} with {} conversations, {} members",
            started.elapsed(),
            store.conversations.len(),
            store.users.len()
        );
        store.refresh(cx);
        store._polling = vec![store.spawn_activity_poll(cx), store.spawn_sweep(cx)];
        store
    }

    // ----------------------------------------------------------- readers

    pub fn client(&self) -> &SlackClient {
        &self.client
    }

    pub fn identity(&self) -> &AuthIdentity {
        &self.identity
    }

    /// Every conversation, including ones the sidebar hides.
    pub fn conversations(&self) -> &[Conversation] {
        &self.conversations
    }

    /// The conversations worth listing: everything except direct messages that
    /// have been confirmed empty.
    pub fn listable(&self) -> impl Iterator<Item = &Conversation> {
        self.conversations.iter().filter(|c| c.is_listable())
    }

    pub fn load_state(&self) -> &LoadState {
        &self.load_state
    }

    pub fn connectivity(&self) -> Connectivity {
        self.connectivity
    }

    /// The cache this workspace persists to, so views can store their own
    /// per-conversation data beside it.
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    pub fn presence(&self) -> Presence {
        self.presence
    }

    pub fn dnd(&self) -> &DndState {
        &self.dnd
    }

    pub fn emoji(&self) -> &EmojiIndex {
        &self.emoji
    }

    pub fn selected_id(&self) -> Option<&SharedString> {
        self.selected.as_ref()
    }

    pub fn selected(&self) -> Option<&Conversation> {
        let id = self.selected.as_ref()?;
        self.conversation(id)
    }

    pub fn conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == id)
    }

    pub fn user(&self, id: &str) -> Option<&User> {
        self.users.get(id)
    }

    /// The name to show for `id`, falling back to the raw id while the
    /// directory is still loading.
    pub fn user_name(&self, id: &str) -> SharedString {
        match self.users.get(id) {
            Some(user) => user.display_name().to_string().into(),
            None => id.to_string().into(),
        }
    }

    pub fn is_me(&self, user_id: &str) -> bool {
        user_id == self.identity.user_id
    }

    /// Total unread across every conversation — the number a window title
    /// wants.
    pub fn total_unread(&self) -> u32 {
        self.conversations.iter().map(|c| c.unread).sum()
    }

    pub fn draft(&self, channel: &str) -> Option<&String> {
        self.drafts.get(channel)
    }

    // ----------------------------------------------------------- commands

    pub fn select(&mut self, id: impl Into<SharedString>, cx: &mut Context<Self>) {
        let id = id.into();
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        self.selected = Some(id);
        self.persist();
        cx.emit(WorkspaceEvent::SelectionChanged);
        cx.notify();
    }

    pub fn set_draft(&mut self, channel: impl Into<SharedString>, text: String) {
        let channel = channel.into();
        if text.trim().is_empty() {
            self.drafts.remove(&channel);
        } else {
            self.drafts.insert(channel, text);
        }
    }

    /// Record that `channel` has been read up to `ts`, locally and in Slack.
    pub fn mark_read(&mut self, channel: SharedString, ts: Ts, cx: &mut Context<Self>) {
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == channel) else {
            return;
        };
        if conversation.last_read.as_f64() >= ts.as_f64() && conversation.unread == 0 {
            return;
        }
        conversation.last_read = ts.clone();
        conversation.unread = 0;
        if conversation
            .latest
            .as_ref()
            .is_none_or(|latest| latest.as_f64() < ts.as_f64())
        {
            conversation.latest = Some(ts.clone());
        }
        conversation.known_empty = false;
        self.sort_conversations();
        self.persist();
        cx.emit(WorkspaceEvent::ConversationsChanged);
        cx.notify();

        let client = self.client.clone();
        cx.background_spawn(async move {
            // A failed mark only means other clients keep their badge; the
            // local view is already correct, so this stays quiet.
            if let Err(err) = client.mark_read(&channel, &ts).await {
                log::debug!("conversations.mark failed for {channel}: {err}");
            }
        })
        .detach();
    }

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

    pub fn set_presence(&mut self, presence: Presence, cx: &mut Context<Self>) {
        self.presence = presence;
        cx.emit(WorkspaceEvent::StatusChanged);
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            if let Err(err) = client.set_presence(presence).await {
                _ = this.update(cx, |_, cx| {
                    cx.emit(WorkspaceEvent::Failed(
                        format!("Could not change your presence: {err}").into(),
                    ));
                });
            }
        })
        .detach();
    }

    pub fn snooze(&mut self, minutes: Option<u32>, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = match minutes {
                Some(minutes) => client.snooze(minutes).await,
                None => client.end_snooze().await,
            };
            _ = this.update(cx, |this, cx| match result {
                Ok(state) => {
                    this.dnd = state;
                    cx.emit(WorkspaceEvent::StatusChanged);
                    cx.notify();
                }
                Err(err) => cx.emit(WorkspaceEvent::Failed(
                    format!("Could not change notifications: {err}").into(),
                )),
            });
        })
        .detach();
    }

    /// Learn about a user the directory did not include (a guest, or a
    /// workspace larger than [`DIRECTORY_LIMIT`]).
    pub fn resolve_user(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if self.users.contains_key(&id) {
            return;
        }
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let Ok(user) = client.user_info(&id).await else {
                return;
            };
            _ = this.update(cx, |this, cx| {
                this.users.insert(id, user);
                this.rename_direct_messages();
                // Remember them, so the next launch does not ask again.
                let directory: Vec<User> = this.users.values().cloned().collect();
                this.cache.write(CACHE_USERS, &directory);
                cx.emit(WorkspaceEvent::DirectoryChanged);
                cx.notify();
            });
        })
        .detach();
    }

    /// Pin or unpin a conversation.
    ///
    /// Slack's own stars are read-only to this client — writing them needs a
    /// scope Slack no longer grants for `stars.add` — so a toggle here is
    /// local, persisted with the rest of the workspace, and merged with
    /// whatever Slack reports.
    pub fn toggle_star(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == id) else {
            return;
        };
        conversation.starred = !conversation.starred;
        self.sort_conversations();
        self.persist();
        cx.emit(WorkspaceEvent::ConversationsChanged);
        cx.notify();
    }

    /// Report a failure raised by a view, so error presentation stays in one
    /// place instead of being duplicated per surface.
    pub fn report(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::Failed(message.into()));
    }

    /// Note that Slack rate-limited a history request, and slow the poll down
    /// to the documented floor for the strictest tier.
    ///
    /// This is one-way on purpose: a workspace that hit the limit once will
    /// hit it again, and flapping between intervals would just burn the quota
    /// that is left.
    pub fn note_rate_limit(&mut self, cx: &mut Context<Self>) {
        if self.activity_interval == ACTIVE_POLL_THROTTLED {
            return;
        }
        log::info!("Slack rate-limited history; polling once a minute from now on");
        self.activity_interval = ACTIVE_POLL_THROTTLED;
        cx.notify();
    }

    /// Apply a locally known new message so the sidebar reorders immediately,
    /// without waiting for the next sweep.
    pub fn note_activity(&mut self, channel: &str, ts: Ts, cx: &mut Context<Self>) {
        let is_active = self.selected.as_deref() == Some(channel);
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == channel) else {
            return;
        };
        conversation.latest = Some(ts.clone());
        conversation.known_empty = false;
        if is_active {
            conversation.last_read = ts;
            conversation.unread = 0;
        } else {
            conversation.unread += 1;
        }
        self.sort_conversations();
        self.persist();
        cx.emit(WorkspaceEvent::ConversationsChanged);
        cx.notify();
    }

    // ----------------------------------------------------------- cache

    /// Populate from disk. Runs before any network call, so the first frame
    /// already has a complete workspace.
    fn restore_from_cache(&mut self) {
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
    fn apply_slack_stars(&mut self, starred: &[String]) {
        for conversation in &mut self.conversations {
            if starred.iter().any(|id| *id == conversation.id) {
                conversation.starred = true;
            }
        }
        self.sort_conversations();
    }

    fn persist(&self) {
        self.cache.write(
            CACHE_WORKSPACE,
            &WorkspaceSnapshot::new(&self.conversations, self.selected.as_ref()),
        );
    }

    // ----------------------------------------------------------- internals

    /// Open something once the list is known, so the window is never empty.
    fn ensure_selection(&mut self, cx: &mut Context<Self>) {
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
    fn apply_channels(&mut self, channels: Vec<Channel>) {
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
    fn rename_direct_messages(&mut self) {
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
    fn sort_conversations(&mut self) {
        self.conversations.sort_by_key(|c| c.name.to_lowercase());
    }

    /// Nudge the active channel view to look for new messages.
    fn spawn_activity_poll(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut interval = ACTIVE_POLL;
            loop {
                cx.background_executor().timer(interval).await;
                let next = this.update(cx, |this, cx| {
                    if this.selected.is_some() {
                        cx.emit(WorkspaceEvent::ActivityPolled);
                    }
                    this.activity_interval
                });
                match next {
                    Ok(next) => interval = next,
                    // The store is gone, so the window is too.
                    Err(_) => return,
                }
            }
        })
    }

    /// Learn the newest timestamp and read marker for a few conversations at a
    /// time, forever, so unread and recency converge and stay converged.
    fn spawn_sweep(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SWEEP_INTERVAL).await;

                let batched = this.update(cx, |this, _| {
                    let batch = this.sweep_batch(now_seconds());
                    if batch.is_empty() {
                        None
                    } else {
                        Some((this.client.clone(), batch))
                    }
                });

                let (client, batch) = match batched {
                    Ok(Some(batched)) => batched,
                    // Nothing to probe this cycle; wait for the next one
                    // rather than ending the sweep for the session.
                    Ok(None) => continue,
                    Err(_) => return,
                };

                let mut probes = Vec::with_capacity(batch.len());
                let mut signed_out = false;
                for (id, needs_read_marker) in batch {
                    match probe(&client, &id, needs_read_marker).await {
                        Ok(probe) => probes.push((id, probe)),
                        Err(err) => {
                            if err.is_auth_failure() {
                                signed_out = true;
                                break;
                            }
                            log::debug!("probe failed for {id}: {err}");
                        }
                    }
                }

                let applied = this.update(cx, |this, cx| {
                    if signed_out {
                        cx.emit(WorkspaceEvent::SignedOut);
                        return;
                    }
                    if probes.is_empty() {
                        return;
                    }
                    this.apply_probes(probes, cx);
                    this.persist();
                    cx.emit(WorkspaceEvent::ConversationsChanged);
                    cx.notify();
                });
                if applied.is_err() || signed_out {
                    return;
                }
            }
        })
    }

    /// The conversations to probe next, and whether each still needs its read
    /// marker fetched.
    fn sweep_batch(&self, now: i64) -> Vec<(SharedString, bool)> {
        let mut due: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|c| c.probed_at == 0 || now - c.probed_at > PROBE_TTL_SECONDS)
            .collect();
        due.sort_by_key(|c| c.probe_priority(now));

        due.into_iter()
            .take(SWEEP_BATCH)
            .map(|c| (c.id.clone(), c.last_read.as_f64() == 0.0))
            .collect()
    }

    fn apply_probes(&mut self, probes: Vec<(SharedString, Probe)>, cx: &mut Context<Self>) {
        let now = now_seconds();
        let mut arrived = false;

        for (id, probe) in probes {
            let is_active = self.selected.as_ref() == Some(&id);
            let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == id) else {
                continue;
            };

            conversation.probed_at = now;
            conversation.known_empty = probe.latest.is_none();
            if let Some(latest) = probe.latest {
                conversation.latest = Some(latest);
            }
            if let Some(last_read) = probe.last_read
                && last_read.as_f64() > conversation.last_read.as_f64()
            {
                conversation.last_read = last_read;
            }

            // The open conversation is read by definition; letting a probe put
            // a badge back on it would fight the reader.
            let was_unread = conversation.unread > 0;
            conversation.unread = if is_active {
                0
            } else {
                unread_from(&conversation.last_read, conversation.latest.as_ref())
            };
            // Only the transition is an arrival; a conversation that was
            // already unread should not sound again on every sweep.
            arrived |= !was_unread && conversation.unread > 0 && !is_active;
        }

        if arrived {
            crate::notify::message_arrived(
                crate::notify::Arrival {
                    is_own: false,
                    is_active: false,
                },
                &self.dnd,
                cx,
            );
        }
        self.sort_conversations();
    }
}

/// What one probe learned about a conversation.
#[derive(Debug, Default)]
struct Probe {
    /// Newest message, or `None` when the conversation has never been used.
    latest: Option<Ts>,
    last_read: Option<Ts>,
}

/// Ask Slack the two things it will tell us about a conversation's unread
/// state: what the newest message is, and where the read marker sits.
///
/// The read marker is only fetched when it is not already known, because it
/// changes rarely and costs a second request.
async fn probe(
    client: &SlackClient,
    id: &str,
    needs_read_marker: bool,
) -> slack_api::Result<Probe> {
    let page = client.conversation_history(id, 1, None).await?;
    let latest = page.messages.last().map(|m| m.ts.clone());

    let last_read = if needs_read_marker {
        client
            .conversation_info(id)
            .await
            .ok()
            .and_then(|channel| channel.last_read)
    } else {
        None
    };

    Ok(Probe { latest, last_read })
}

/// Whether a conversation has anything newer than the read marker.
///
/// Slack gives no count, so this is the honest answer: one, meaning "there is
/// something", rather than a number invented from data we do not have.
fn unread_from(last_read: &Ts, latest: Option<&Ts>) -> u32 {
    match latest {
        Some(latest) if latest.as_f64() > last_read.as_f64() => 1,
        _ => 0,
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(id: &str, name: &str, unread: u32, latest: &str) -> Conversation {
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
    fn a_starred_conversation_leaves_its_usual_section() {
        let mut dm = conversation("D1", "ada", 0, "0");
        dm.kind = ChannelKind::Im;
        assert_eq!(dm.section(), Section::DirectMessages);

        dm.starred = true;
        assert_eq!(dm.section(), Section::Starred);
    }

    #[test]
    fn an_unstarred_channel_stays_in_channels() {
        assert_eq!(
            conversation("C1", "general", 0, "0").section(),
            Section::Channels
        );
    }

    #[test]
    fn a_message_newer_than_the_read_marker_counts_as_unread() {
        let read = Ts("1700000100.000100".into());
        assert_eq!(unread_from(&read, Some(&Ts("1700000900.0".into()))), 1);
        assert_eq!(unread_from(&read, Some(&Ts("1700000100.000100".into()))), 0);
        assert_eq!(unread_from(&read, Some(&Ts("1699999999.0".into()))), 0);
    }

    #[test]
    fn a_conversation_that_was_never_probed_is_not_reported_unread() {
        assert_eq!(unread_from(&Ts::default(), None), 0);
    }

    #[test]
    fn an_empty_direct_message_is_hidden_but_an_empty_channel_is_not() {
        let mut dm = conversation("D1", "ada", 0, "0");
        dm.kind = ChannelKind::Im;
        dm.known_empty = true;
        assert!(!dm.is_listable());

        let mut channel = conversation("C1", "general", 0, "0");
        channel.known_empty = true;
        assert!(channel.is_listable());
    }

    #[test]
    fn the_sweep_probes_unseen_direct_messages_before_unseen_channels() {
        let now = 1_700_000_000;
        let mut dm = conversation("D1", "ada", 0, "0");
        dm.kind = ChannelKind::Im;
        let channel = conversation("C1", "general", 0, "0");

        assert!(dm.probe_priority(now) < channel.probe_priority(now));
    }

    #[test]
    fn the_sweep_prefers_anything_unprobed_over_a_stale_refresh() {
        let now = 1_700_000_000;
        let unprobed = conversation("C1", "general", 0, "0");
        let mut stale = conversation("C2", "random", 0, "0");
        stale.probed_at = now - 10_000;

        assert!(unprobed.probe_priority(now) < stale.probe_priority(now));
    }

    #[test]
    fn among_probed_conversations_the_oldest_probe_goes_first() {
        let now = 1_700_000_000;
        let mut older = conversation("C1", "general", 0, "0");
        older.probed_at = now - 9_000;
        let mut newer = conversation("C2", "random", 0, "0");
        newer.probed_at = now - 100;

        assert!(older.probe_priority(now) < newer.probe_priority(now));
    }
}
