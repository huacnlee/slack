//! Getting a conversation's messages, and keeping them.
//!
//! Three fetches with different jobs: the first page when a conversation
//! opens, older pages as the reader scrolls back, and whatever arrived since.
//! Each one writes through to the cache, so the next open starts from disk
//! rather than from the network.

use super::*;

impl ChannelView {
    pub(super) fn fetch_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        let client = self.store.read(cx).client().clone();
        let revision = self.revision;

        cx.spawn_in(window, async move |this, cx| {
            let page = client.conversation_history(&channel, PAGE_SIZE, None).await;

            _ = this.update_in(cx, |this, _, cx| {
                if this.revision != revision {
                    return;
                }
                match page {
                    Ok(page) => {
                        this.has_more = page.has_more;
                        this.transcript.replace(page.messages);
                        this.state = LoadState::Ready;
                        this.rebuild_rows(cx);
                        this.scroll_to_tail();
                        this.persist(cx);
                        this.resolve_unknown_authors(cx);
                        this.fetch_thumbnails(cx);
                        this.mark_read(cx);
                        this.scroll_to_tail();
                    }
                    Err(err) => {
                        // Cached messages are more use than an error page.
                        this.state = if this.transcript.is_empty() {
                            LoadState::Failed(err.to_string().into())
                        } else {
                            log::info!("history failed, showing cached messages: {err}");
                            LoadState::Stale
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn fetch_older(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(channel), Some(oldest)) =
            (self.channel.clone(), self.transcript.first_ts().cloned())
        else {
            return;
        };
        if self.loading_older {
            return;
        }
        self.loading_older = true;
        cx.notify();

        let client = self.store.read(cx).client().clone();
        let revision = self.revision;

        cx.spawn(async move |this, cx| {
            let page = client
                .conversation_history(&channel, OLDER_PAGE_SIZE, Some(&oldest))
                .await;

            _ = this.update(cx, |this, cx| {
                this.loading_older = false;
                if this.revision != revision {
                    return;
                }
                match page {
                    Ok(page) => {
                        this.has_more = page.has_more;
                        this.transcript.prepend(page.messages);
                        this.rebuild_rows(cx);
                    }
                    Err(err) => cx.emit(ChannelEvent::Failed(
                        format!("Could not load earlier messages: {err}").into(),
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Pick up anything posted since the newest message on screen.
    pub(super) fn fetch_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(channel), Some(latest)) =
            (self.channel.clone(), self.transcript.last_ts().cloned())
        else {
            return;
        };
        if self.state != LoadState::Ready {
            return;
        }

        let client = self.store.read(cx).client().clone();
        let revision = self.revision;

        cx.spawn_in(window, async move |this, cx| {
            let page = match client
                .conversation_history_since(&channel, &latest, PAGE_SIZE)
                .await
            {
                Ok(page) => page,
                Err(err) => {
                    // Slack throttles history hard for newer apps; tell the
                    // store so every poll after this one asks less often.
                    if matches!(err, slack_api::Error::RateLimited(_))
                        || err.slack_code() == Some("ratelimited")
                    {
                        _ = this.update(cx, |this, cx| {
                            this.store.update(cx, |store, cx| store.note_rate_limit(cx));
                        });
                    }
                    return;
                }
            };
            if page.messages.is_empty() {
                return;
            }

            _ = this.update_in(cx, |this, _, cx| {
                if this.revision != revision {
                    return;
                }
                let newest = page.messages.last().map(|m| m.ts.clone());
                this.transcript.merge(page.messages);
                this.rebuild_rows(cx);
                if let Some(ts) = newest {
                    this.store
                        .update(cx, |store, cx| store.note_activity(&channel, ts, cx));
                }
                this.state = LoadState::Ready;
                this.persist(cx);
                this.mark_read(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Pull down the image attachments on screen.
    ///
    /// Slack serves thumbnails from a host that requires the token, so they
    /// are fetched with the API client and cached rather than handed to the
    /// image loader as URLs.
    pub(super) fn fetch_thumbnails(&mut self, cx: &mut Context<Self>) {
        let files: Vec<slack_api::models::File> = self
            .transcript
            .entries()
            .iter()
            .flat_map(|entry| entry.message.files.iter().cloned())
            .collect();
        let wanted = attachments::wanted(files, &self.thumbnails);
        if wanted.is_empty() {
            return;
        }

        let store = self.store.read(cx);
        let client = store.client().clone();
        let cache = store.cache().clone();

        cx.spawn(async move |this, cx| {
            for file in wanted {
                let Some(path) = attachments::fetch(&cache, &client, &file).await else {
                    continue;
                };
                let updated = this.update(cx, |this, cx| {
                    this.thumbnails.insert(file.id.clone(), path);
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Look up authors the workspace directory does not have.
    ///
    /// `users.list` is capped, and it omits members who have left, so a
    /// transcript regularly names people the directory has never heard of.
    /// Without this they render as a raw id. Done here rather than in `render`
    /// because a lookup is a request, and rendering must not make requests.
    pub(super) fn resolve_unknown_authors(&mut self, cx: &mut Context<Self>) {
        let unknown: Vec<SharedString> = {
            let store = self.store.read(cx);
            let mut unknown: Vec<SharedString> = self
                .transcript
                .entries()
                .iter()
                .filter_map(|entry| entry.message.user.as_deref())
                .filter(|id| store.user(id).is_none())
                .map(SharedString::from)
                .collect();
            unknown.sort();
            unknown.dedup();
            unknown
        };

        // One request each; a page of history rarely has more than a few.
        for id in unknown.into_iter().take(MAX_AUTHOR_LOOKUPS) {
            self.store
                .update(cx, |store, cx| store.resolve_user(id, cx));
        }
    }

    /// Where this conversation's transcript lives on disk.
    pub(super) fn cache_key(channel: &str) -> String {
        format!("messages/{channel}")
    }

    /// Remember the tail of the transcript for the next launch.
    pub(super) fn persist(&self, cx: &Context<Self>) {
        let Some(channel) = self.channel.as_ref() else {
            return;
        };
        let entries = self.transcript.entries();
        let tail: Vec<&Message> = entries
            .iter()
            .rev()
            .take(CACHED_MESSAGES)
            .rev()
            .map(|entry| &entry.message)
            .collect();
        self.store
            .read(cx)
            .cache()
            .write(&Self::cache_key(channel), &tail);
    }

    pub(super) fn mark_read(&mut self, cx: &mut Context<Self>) {
        let (Some(channel), Some(ts)) = (self.channel.clone(), self.transcript.last_ts().cloned())
        else {
            return;
        };
        self.store
            .update(cx, |store, cx| store.mark_read(channel, ts, cx));
    }
}
