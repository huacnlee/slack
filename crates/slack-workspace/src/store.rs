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

mod conversation;
mod realtime;
mod refresh;
mod sweep;

pub use conversation::{Connectivity, Conversation, LoadState, Section, matching};
pub use realtime::RealtimeState;
use realtime::Typist;

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{AppContext as _, Context, EventEmitter, SharedString, Task};

use slack_api::emoji::EmojiIndex;
use slack_api::models::{
    AuthIdentity, Channel, ChannelKind, DndState, Message, Presence, Ts, User,
};
use slack_api::{ALL_CONVERSATION_TYPES, Cache, RtmEvent, SlackClient};

use crate::snapshot::WorkspaceSnapshot;

/// How often the active conversation is checked for new messages.
///
/// Slack apps created after May 2025 are limited to one `conversations.history`
/// request per minute. Older apps get a far higher tier, so the client starts
/// responsive and falls back to [`ACTIVE_POLL_THROTTLED`] the first time Slack
/// says it is asking too often — rather than assuming the worst for everyone.
const ACTIVE_POLL: Duration = Duration::from_secs(6);
const ACTIVE_POLL_THROTTLED: Duration = Duration::from_secs(65);

/// How often the background sweep learns more about the conversation list.
const SWEEP_INTERVAL: Duration = Duration::from_secs(20);
/// Conversations probed per sweep cycle. Each costs one or two API calls, so
/// this is the knob that keeps a large workspace inside Slack's rate limit
/// while still converging in a few minutes.
const SWEEP_BATCH: usize = 1;
/// How long a probe stays trusted before the sweep refreshes it.
/// How long the sweep waits once Slack has refused a history request.
///
/// At this point the quota is the scarce thing, and the conversation on screen
/// has first claim on it.
const SWEEP_THROTTLED: Duration = Duration::from_secs(120);

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
    /// Something happened in the workspace, as it happened. The store has
    /// already applied what it owns; this is for whoever is showing the
    /// messages themselves.
    Realtime(RtmEvent),
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
    realtime: RealtimeState,
    /// Whether Slack has refused a history request this session.
    throttled: bool,
    /// Who is typing where. Entries expire rather than being cleared, since
    /// Slack never says anyone stopped.
    typing: HashMap<SharedString, Vec<Typist>>,
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
            realtime: RealtimeState::Connecting,
            throttled: false,
            typing: HashMap::new(),
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
        // The socket does the real work; the poll and the sweep stay as the
        // floor for a token Slack will not open one for.
        store._polling = vec![
            store.spawn_realtime(cx),
            store.spawn_activity_poll(cx),
            store.spawn_sweep(cx),
        ];
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
    /// Slack refused a history request.
    ///
    /// The client paces itself per method, so this is not about avoiding the
    /// next 429 — it is about who gets the requests that remain. Backing the
    /// sweep off as well as the poll means the conversation someone is reading
    /// is not queued behind a background scan of one they are not.
    pub fn note_rate_limit(&mut self, cx: &mut Context<Self>) {
        if self.throttled {
            return;
        }
        log::info!("Slack rate-limited history; polling once a minute and sweeping rarely");
        self.throttled = true;
        self.activity_interval = ACTIVE_POLL_THROTTLED;
        cx.notify();
    }

    /// How long the sweep should wait before its next probe.
    ///
    /// Nothing at all while the socket is live: arrivals and read markers
    /// come through as events, so probing for them spends a request to learn
    /// something already known.
    pub(crate) fn sweep_interval(&self) -> Option<Duration> {
        sweep::interval(&self.realtime, self.throttled)
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
}
