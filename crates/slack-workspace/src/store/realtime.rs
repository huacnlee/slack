//! Keeping the workspace current from Slack's event stream.
//!
//! The sweep in [`super::sweep`] exists because a client with no socket has to
//! go looking. With one, arrivals announce themselves: the sidebar lights up
//! as the message lands rather than up to a sweep away, and the transcript on
//! screen grows without asking. The sweep stays as the floor — a token without
//! `rtm:stream`, or a socket that will not open, still gets a working client,
//! just a slower one.

use std::time::Duration;

use super::sweep::{now_seconds, unread_from};
use super::*;

/// How long one `user_typing` event stands for.
///
/// Slack repeats the event every few seconds while someone is still typing and
/// never says they stopped, so the indicator has to expire on its own. Five
/// seconds outlasts the repeat without leaving a ghost after they walk away.
const TYPING_TTL: i64 = 5;

/// Where the event stream stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeState {
    /// Opening the socket for the first time.
    Connecting,
    /// Events are arriving as they happen.
    Live,
    /// The socket dropped; reopening.
    Reconnecting,
    /// Slack will not open a socket for this token. Polling is doing the work,
    /// and the reason says why it has to.
    Polling(SharedString),
}

impl RealtimeState {
    /// Whether arrivals can be trusted to announce themselves.
    pub fn is_live(&self) -> bool {
        *self == RealtimeState::Live
    }
}

/// Someone typing, and when they were last seen doing it.
#[derive(Debug, Clone)]
pub(super) struct Typist {
    pub user: SharedString,
    pub seen_at: i64,
}

impl WorkspaceStore {
    /// Consume the event stream for as long as this store lives.
    pub(super) fn spawn_realtime(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let (sender, mut events) = self.client.realtime();
        self.typing_sender = Some(sender);

        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                // An error here means the window closed, which also drops the
                // receiver and so closes the socket.
                if this
                    .update(cx, |this, cx| this.apply_realtime(event, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
    }

    /// Apply one event, then pass it on for the open conversation to use.
    fn apply_realtime(&mut self, event: RtmEvent, cx: &mut Context<Self>) {
        match &event {
            RtmEvent::Connected => {
                let resuming = self.realtime != RealtimeState::Connecting;
                self.realtime = RealtimeState::Live;
                self.connectivity = Connectivity::Online;
                // Nothing is replayed across a gap. On a reconnect the only
                // honest thing to do is go and look at what was missed.
                if resuming {
                    self.refresh(cx);
                }
                cx.emit(WorkspaceEvent::StatusChanged);
            }

            RtmEvent::Disconnected => {
                self.realtime = RealtimeState::Reconnecting;
                self.typing.clear();
                cx.emit(WorkspaceEvent::StatusChanged);
            }

            RtmEvent::Stopped(reason) => {
                log::warn!("no realtime stream ({reason}); falling back to polling");
                self.realtime = RealtimeState::Polling(reason.clone().into());
                cx.emit(WorkspaceEvent::StatusChanged);
            }

            RtmEvent::Posted { channel, message } => {
                self.note_posted(channel, message, cx);
            }

            RtmEvent::ReadMarker { channel, ts } => {
                // The reader caught up somewhere else — their phone, or the
                // official client. This window should agree.
                if let Some(conversation) = self
                    .conversations
                    .iter_mut()
                    .find(|c| c.id == channel.as_str())
                {
                    conversation.last_read = ts.clone();
                    conversation.unread =
                        unread_from(&conversation.last_read, conversation.latest.as_ref());
                    self.persist();
                    cx.emit(WorkspaceEvent::ConversationsChanged);
                }
            }

            RtmEvent::Typing { channel, user } => self.note_typing(channel, user, cx),

            RtmEvent::PresenceChanged { user, presence } => {
                if self.is_me(user) {
                    self.presence = match presence.as_str() {
                        "away" => Presence::Away,
                        _ => Presence::Active,
                    };
                    cx.emit(WorkspaceEvent::StatusChanged);
                }
            }

            // Coarse on purpose: these are rare, and reloading the shape is
            // both simpler and more correct than patching each case.
            RtmEvent::WorkspaceChanged => self.refresh(cx),

            RtmEvent::Edited { .. }
            | RtmEvent::Deleted { .. }
            | RtmEvent::ReactionChanged { .. } => {}
        }

        // The open conversation applies these to its own transcript; the store
        // has no view of which messages are on screen.
        cx.emit(WorkspaceEvent::Realtime(event));
        cx.notify();
    }

    /// A message arrived. Decide what it means for the sidebar, and whether it
    /// is worth a sound.
    fn note_posted(&mut self, channel: &str, message: &Message, cx: &mut Context<Self>) {
        let is_active = self.selected.as_deref() == Some(channel);
        let is_own = message.user.as_deref().is_some_and(|user| self.is_me(user));

        // Someone typing has, by definition, stopped.
        if let Some(user) = &message.user {
            self.forget_typist(channel, user);
        }

        let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == channel) else {
            // A conversation this window has never heard of — a new DM, or a
            // channel just joined. Reload the list so it can be shown.
            self.refresh(cx);
            return;
        };

        // The same message can reach us twice: once over the socket and once
        // from the poll that was already in flight. Counting it twice would
        // put a badge on a conversation the reader is looking at.
        if conversation.latest.as_ref() == Some(&message.ts) {
            return;
        }

        conversation.latest = Some(message.ts.clone());
        conversation.known_empty = false;
        conversation.probed_at = now_seconds();

        if is_active || is_own {
            // Their own message from another client is already read, and the
            // conversation on screen is read by definition.
            conversation.last_read = message.ts.clone();
            conversation.unread = 0;
        } else {
            conversation.unread = 1;
        }

        self.sort_conversations();
        self.persist();

        slack_ui::notify::message_arrived(
            slack_ui::notify::Arrival { is_own, is_active },
            &self.dnd,
            cx,
        );
        cx.emit(WorkspaceEvent::ConversationsChanged);
    }

    /// Record that someone is typing, and arrange for that to stop being true.
    fn note_typing(&mut self, channel: &str, user: &str, cx: &mut Context<Self>) {
        if self.is_me(user) {
            return;
        }

        let now = now_seconds();
        let typists = self.typing.entry(channel.into()).or_default();
        match typists.iter_mut().find(|t| t.user == user) {
            Some(typist) => typist.seen_at = now,
            None => typists.push(Typist {
                user: user.into(),
                seen_at: now,
            }),
        }

        // Nothing else would notice the entry going stale, so wake up once
        // when it does and let the indicator clear itself.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(TYPING_TTL as u64 + 1))
                .await;
            _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();

        cx.notify();
    }

    fn forget_typist(&mut self, channel: &str, user: &str) {
        if let Some(typists) = self.typing.get_mut(channel) {
            typists.retain(|t| t.user != user);
        }
    }

    /// Say that the reader is typing here.
    ///
    /// Safe to call on every keystroke — the sender decides how often that is
    /// worth putting on the wire, and does nothing at all when there is no
    /// socket, which is the same thing it would mean to the people watching.
    pub fn typing_in(&self, channel: &str) {
        if let Some(sender) = &self.typing_sender {
            sender.typing(channel);
        }
    }

    /// Who is currently typing in a conversation, by display name.
    pub fn typing(&self, channel: &str) -> Vec<SharedString> {
        let cutoff = now_seconds() - TYPING_TTL;
        self.typing
            .get(channel)
            .map(|typists| {
                typists
                    .iter()
                    .filter(|t| t.seen_at >= cutoff)
                    .map(|t| self.user_name(&t.user))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where the event stream stands, for the reader to see.
    pub fn realtime(&self) -> &RealtimeState {
        &self.realtime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typist_expires_without_being_told_they_stopped() {
        let mut typing: Vec<Typist> = vec![
            Typist {
                user: "U1".into(),
                seen_at: now_seconds(),
            },
            Typist {
                user: "U2".into(),
                seen_at: now_seconds() - 60,
            },
        ];

        let cutoff = now_seconds() - TYPING_TTL;
        typing.retain(|t| t.seen_at >= cutoff);

        assert_eq!(typing.len(), 1, "only the recent typist should survive");
        assert_eq!(typing[0].user, "U1");
    }

    #[test]
    fn the_stream_is_only_trusted_once_it_is_live() {
        assert!(RealtimeState::Live.is_live());
        assert!(!RealtimeState::Connecting.is_live());
        assert!(!RealtimeState::Reconnecting.is_live());
        assert!(!RealtimeState::Polling("missing_scope".into()).is_live());
    }
}
