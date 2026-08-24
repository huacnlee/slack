//! What the reader can do to a message.
//!
//! Each one is the whole of its command: it updates what is on screen, tells
//! Slack, and puts the optimistic change back if Slack disagrees. A command
//! modelled in one place cannot drift between the keyboard and the menu.

use super::*;

impl ChannelView {
    /// Post `text` to the open conversation.
    ///
    /// The command the composer invokes, and the seam anything else that wants
    /// to post here goes through — a quick reply from the activity list, an
    /// end-to-end test — so there is one path to Slack and not several.
    pub fn send(&mut self, text: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        let client = self.store.read(cx).client().clone();
        let revision = self.revision;

        cx.spawn_in(window, async move |this, cx| {
            let result = client.post_message(&channel, &text, None).await;

            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    this.composer
                        .update(cx, |composer, cx| composer.accept(window, cx));
                    this.store.update(cx, |store, _| {
                        store.set_draft(channel.clone(), String::new())
                    });
                    if this.revision == revision {
                        this.fetch_new(window, cx);
                        this.scroll_to_tail();
                    }
                }
                Err(err) => {
                    this.composer.update(cx, |composer, cx| composer.reject(cx));
                    cx.emit(ChannelEvent::Failed(
                        format!("Could not send that message: {err}").into(),
                    ));
                }
            });
        })
        .detach();
    }

    pub(super) fn save_edit(
        &mut self,
        ts: Ts,
        text: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        let client = self.store.read(cx).client().clone();

        cx.spawn_in(window, async move |this, cx| {
            let result = client.update_message(&channel, &ts, &text).await;

            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.editing = None;
                    this.fetch_latest(window, cx);
                }
                Err(err) => {
                    if let Some(session) = &this.editing {
                        session
                            .composer
                            .update(cx, |composer, cx| composer.reject(cx));
                    }
                    cx.emit(ChannelEvent::Failed(
                        format!("Could not save that edit: {err}").into(),
                    ));
                }
            });
        })
        .detach();
    }

    pub(super) fn confirm_delete(&mut self, ts: Ts, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let channel = channel.clone();
            let ts = ts.clone();

            alert
                .title("Delete this message?")
                .description("It will be removed for everyone in the conversation.")
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger)
                        .on_ok(move |_, _, cx| {
                            _ = view.update(cx, |this, cx| {
                                this.delete(channel.clone(), ts.clone(), cx)
                            });
                            true
                        }),
                )
        });
    }

    pub(super) fn delete(&mut self, channel: SharedString, ts: Ts, cx: &mut Context<Self>) {
        let client = self.store.read(cx).client().clone();
        // Remove it locally first: the row is gone from the reader's view the
        // moment they confirm, and a failed call puts it back.
        self.transcript.remove(&ts);
        self.rebuild_rows(cx);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = client.delete_message(&channel, &ts).await;
            _ = this.update(cx, |this, cx| {
                if let Err(err) = result {
                    cx.emit(ChannelEvent::Failed(
                        format!("Could not delete that message: {err}").into(),
                    ));
                    this.state = LoadState::Ready;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn toggle_reaction(
        &mut self,
        ts: Ts,
        name: SharedString,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        let store = self.store.read(cx);
        let me = SharedString::from(store.identity().user_id.clone());
        let client = store.client().clone();

        let Some(entry) = self.transcript.get(&ts) else {
            return;
        };
        let mine = entry
            .message
            .reactions
            .iter()
            .any(|r| r.name == name.as_ref() && r.users.iter().any(|u| *u == me));

        // Show the change immediately; the refresh below reconciles it.
        let mut reactions = entry.message.reactions.clone();
        match reactions.iter_mut().find(|r| r.name == name.as_ref()) {
            Some(reaction) if mine => {
                reaction.count = reaction.count.saturating_sub(1);
                reaction.users.retain(|u| *u != me);
            }
            Some(reaction) => {
                reaction.count += 1;
                reaction.users.push(me.to_string());
            }
            None => reactions.push(slack_api::models::Reaction {
                name: name.to_string(),
                count: 1,
                users: vec![me.to_string()],
            }),
        }
        reactions.retain(|r| r.count > 0);
        self.transcript.set_reactions(&ts, reactions);
        // The row is a different height now; the list caches heights, so it
        // has to be told rather than merely redrawn.
        self.invalidate_row(&ts);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = if mine {
                client.remove_reaction(&channel, &ts, &name).await
            } else {
                client.add_reaction(&channel, &ts, &name).await
            };

            if let Err(err) = result {
                // `already_reacted` and `no_reaction` mean the server already
                // agrees with what was just drawn; nothing to report.
                let benign = matches!(err.slack_code(), Some("already_reacted" | "no_reaction"));
                if !benign {
                    _ = this.update(cx, |_, cx| {
                        cx.emit(ChannelEvent::Failed(
                            format!("Could not change that reaction: {err}").into(),
                        ))
                    });
                }
            }
        })
        .detach();
    }

    pub(super) fn copy_link(&mut self, ts: Ts, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        let client = self.store.read(cx).client().clone();

        cx.spawn(async move |this, cx| {
            let result = client.message_permalink(&channel, &ts).await;
            _ = this.update(cx, |_, cx| match result {
                Ok(link) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(link));
                    cx.emit(ChannelEvent::Failed("Link copied".into()));
                }
                Err(err) => cx.emit(ChannelEvent::Failed(
                    format!("Could not copy that link: {err}").into(),
                )),
            });
        })
        .detach();
    }

    pub(super) fn start_edit(&mut self, ts: Ts, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.transcript.get(&ts) else {
            return;
        };
        let text = entry.message.text.clone();

        let composer = cx.new(|cx| {
            let mut composer = Composer::new("Edit this message", ComposerMode::Edit, window, cx);
            composer.set_text(&text, window, cx);
            composer
        });
        let subscription = cx.subscribe_in(&composer, window, {
            let ts = ts.clone();
            move |this, composer, event, window, cx| match event {
                ComposerEvent::Submit(text) => this.save_edit(ts.clone(), text.clone(), window, cx),
                ComposerEvent::Cancel => {
                    if let Some(session) = this.editing.take() {
                        this.invalidate_row(&session.ts);
                    }
                    cx.notify();
                }
                _ => {
                    let _ = composer;
                }
            }
        });
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        self.invalidate_row(&ts);
        self.editing = Some(EditSession {
            ts,
            composer,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Pick a file and share it into the open conversation.
    ///
    /// The composer's current text rides along as the file's comment, which is
    /// what Slack does and what makes "here is the log" one message instead of
    /// two.
    pub(super) fn attach_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.clone() else {
            return;
        };
        if self.uploading.is_some() {
            return;
        }

        let client = self.store.read(cx).client().clone();
        let comment = self.composer.read(cx).text(cx).to_string();
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Share".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            // A cancelled picker is an ordinary outcome, not a failure.
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let name: SharedString = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "attachment".to_string())
                .into();

            _ = this.update(cx, |this, cx| {
                this.uploading = Some(name.clone());
                cx.notify();
            });

            // Reading from disk belongs off the main thread.
            let read = cx
                .background_spawn(async move { std::fs::read(&path) })
                .await;

            let result = match read {
                Ok(bytes) => client
                    .upload_file(&channel, &name, bytes, Some(&comment), None)
                    .await
                    .map(|_| ())
                    .map_err(|err| err.to_string()),
                Err(err) => Err(format!("could not read that file: {err}")),
            };

            _ = this.update_in(cx, |this, window, cx| {
                this.uploading = None;
                match result {
                    Ok(()) => {
                        this.composer
                            .update(cx, |composer, cx| composer.accept(window, cx));
                        this.fetch_new(window, cx);
                    }
                    Err(message) => cx.emit(ChannelEvent::Failed(
                        format!("Could not share {name}: {message}").into(),
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
