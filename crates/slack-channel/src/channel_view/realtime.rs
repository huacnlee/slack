//! Applying live events to the conversation on screen.
//!
//! The store decides what an event means for the workspace — which
//! conversation lit up, who is typing. This decides what it means for the
//! transcript: a message appears, changes, or goes away without anyone having
//! asked Slack a question.
//!
//! Events for other conversations are ignored here rather than filtered
//! upstream, because the store has no idea which messages are on screen.

use slack_api::RtmEvent;
use slack_api::models::Reaction;

use super::*;

impl ChannelView {
    /// Fold one live event into the transcript.
    pub(super) fn apply_realtime(
        &mut self,
        event: &RtmEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A reconnect means the socket was silent for a while and nothing is
        // replayed, so the only honest move is to go and look.
        if matches!(event, RtmEvent::Connected) {
            self.fetch_new(window, cx);
            return;
        }

        let Some(open) = self.channel.clone() else {
            return;
        };
        let for_us = |channel: &String| channel.as_str() == open.as_ref();

        match event {
            RtmEvent::Posted { channel, message } if for_us(channel) => {
                // `merge` is keyed on the timestamp, so the copy that arrives
                // over the socket and the copy the poll was already fetching
                // settle as one message.
                self.transcript.merge(vec![(**message).clone()]);
                self.rebuild_rows(cx);
                self.persist(cx);
                self.mark_read(cx);
                cx.notify();
            }

            RtmEvent::Edited { channel, message } if for_us(channel) => {
                self.transcript
                    .set_text(&message.ts, message.text.clone(), message.edited.clone());
                self.invalidate_row(&message.ts);
                self.persist(cx);
                cx.notify();
            }

            RtmEvent::Deleted { channel, ts } if for_us(channel) => {
                // If it is the message being edited, that edit has nothing
                // left to save.
                if self.editing.as_ref().is_some_and(|s| s.ts == *ts) {
                    self.cancel_edit(cx);
                }
                self.transcript.remove(ts);
                self.rebuild_rows(cx);
                self.persist(cx);
                cx.notify();
            }

            RtmEvent::ReactionChanged {
                channel,
                ts,
                user,
                name,
                added,
            } if for_us(channel) => {
                let Some(entry) = self.transcript.get(ts) else {
                    return;
                };
                let reactions = apply_reaction(&entry.message.reactions, name, user, *added);
                self.transcript.set_reactions(ts, reactions);
                self.invalidate_row(ts);
                self.persist(cx);
                cx.notify();
            }

            // Typing lives on the store; the indicator re-reads it on notify.
            RtmEvent::Typing { channel, .. } if for_us(channel) => cx.notify(),

            _ => {}
        }
    }
}

/// One person's reaction, added or removed.
///
/// Slack reports the change, not the resulting list, so the count has to be
/// kept here. Doing it by user id rather than by incrementing means the same
/// event arriving twice cannot inflate the tally.
fn apply_reaction(current: &[Reaction], name: &str, user: &str, added: bool) -> Vec<Reaction> {
    let mut reactions = current.to_vec();

    match reactions.iter_mut().find(|r| r.name == name) {
        Some(reaction) => {
            if added {
                if !reaction.users.iter().any(|u| u == user) {
                    reaction.users.push(user.to_string());
                }
            } else {
                reaction.users.retain(|u| u != user);
            }
            reaction.count = reaction.users.len() as u32;
        }
        None if added => reactions.push(Reaction {
            name: name.to_string(),
            count: 1,
            users: vec![user.to_string()],
        }),
        None => {}
    }

    // A reaction nobody holds is not a reaction.
    reactions.retain(|r| r.count > 0);
    reactions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reaction(name: &str, users: &[&str]) -> Reaction {
        Reaction {
            name: name.to_string(),
            count: users.len() as u32,
            users: users.iter().map(|u| u.to_string()).collect(),
        }
    }

    #[test]
    fn a_new_reaction_starts_at_one() {
        let after = apply_reaction(&[], "tada", "U1", true);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].count, 1);
    }

    #[test]
    fn the_same_event_twice_does_not_inflate_the_count() {
        let before = vec![reaction("tada", &["U1"])];
        let once = apply_reaction(&before, "tada", "U2", true);
        let twice = apply_reaction(&once, "tada", "U2", true);

        assert_eq!(twice[0].count, 2, "U2 should be counted once, not twice");
    }

    #[test]
    fn removing_the_last_holder_removes_the_reaction() {
        let before = vec![reaction("tada", &["U1"])];
        assert!(apply_reaction(&before, "tada", "U1", false).is_empty());
    }

    #[test]
    fn removing_someone_who_never_reacted_changes_nothing() {
        let before = vec![reaction("tada", &["U1"])];
        let after = apply_reaction(&before, "tada", "U2", false);
        assert_eq!(after[0].count, 1);
        assert_eq!(after[0].users, vec!["U1"]);
    }
}
