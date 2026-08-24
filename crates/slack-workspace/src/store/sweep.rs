//! Learning what is unread, a few conversations at a time.
//!
//! A user token cannot ask Slack for unread counts, so the store works them
//! out: it compares each conversation's read marker against its newest
//! message, and finds that newest message by probing conversations in the
//! background at a rate the rate limiter tolerates.

use super::*;

impl WorkspaceStore {
    /// Nudge the active channel view to look for new messages.
    pub(super) fn spawn_activity_poll(&self, cx: &mut Context<Self>) -> Task<()> {
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
    pub(super) fn spawn_sweep(&self, cx: &mut Context<Self>) -> Task<()> {
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
    pub(super) fn sweep_batch(&self, now: i64) -> Vec<(SharedString, bool)> {
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

    pub(super) fn apply_probes(
        &mut self,
        probes: Vec<(SharedString, Probe)>,
        cx: &mut Context<Self>,
    ) {
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
            slack_ui::notify::message_arrived(
                slack_ui::notify::Arrival {
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
pub(super) struct Probe {
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
pub(super) fn unread_from(last_read: &Ts, latest: Option<&Ts>) -> u32 {
    match latest {
        Some(latest) if latest.as_f64() > last_read.as_f64() => 1,
        _ => 0,
    }
}

pub(super) fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(in crate::store) mod tests {
    use super::*;

    use super::super::conversation::tests::a_conversation;

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
    fn the_sweep_probes_unseen_direct_messages_before_unseen_channels() {
        let now = 1_700_000_000;
        let mut dm = a_conversation("D1", "ada", 0, "0");
        dm.kind = ChannelKind::Im;
        let channel = a_conversation("C1", "general", 0, "0");

        assert!(dm.probe_priority(now) < channel.probe_priority(now));
    }

    #[test]
    fn the_sweep_prefers_anything_unprobed_over_a_stale_refresh() {
        let now = 1_700_000_000;
        let unprobed = a_conversation("C1", "general", 0, "0");
        let mut stale = a_conversation("C2", "random", 0, "0");
        stale.probed_at = now - 10_000;

        assert!(unprobed.probe_priority(now) < stale.probe_priority(now));
    }

    #[test]
    fn among_probed_conversations_the_oldest_probe_goes_first() {
        let now = 1_700_000_000;
        let mut older = a_conversation("C1", "general", 0, "0");
        older.probed_at = now - 9_000;
        let mut newer = a_conversation("C2", "random", 0, "0");
        newer.probed_at = now - 100;

        assert!(older.probe_priority(now) < newer.probe_priority(now));
    }
}
