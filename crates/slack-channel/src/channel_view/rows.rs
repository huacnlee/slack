//! Turning a transcript into the rows a list can measure.
//!
//! The list is virtualised, so it asks for rows by index and needs their
//! count up front. `rebuild_rows` decides the shape — where days break, which
//! messages group under one author, which joins collapse into a notice — and
//! `render_row` renders whatever that decided.

use super::*;

/// One row of the rendered transcript, as the list addresses them.
///
/// Timestamps rather than messages: the list asks for a row long after the
/// shape was decided, and the message it names may have been edited since.
#[derive(Debug, Clone)]
pub(super) enum Row {
    LoadMore,
    Day(SharedString),
    /// A run of membership notices, collapsed into one line.
    Joins(Vec<Ts>),
    Message {
        ts: Ts,
        /// A continuation of the message above.
        grouped: bool,
        /// The first message the reader has not seen.
        unread: bool,
    },
    /// Nothing to show yet.
    Empty,
}

/// "Ada joined", "Ada and Bob joined", "Ada, Bob and 5 others joined".
///
/// Naming the first two keeps the line useful — you usually care that someone
/// specific arrived — while the count keeps it one line however many did.
pub(super) fn summarise_joins(names: &[SharedString]) -> String {
    match names.len() {
        0 => "Someone joined the channel".to_string(),
        1 => format!("{} joined", names[0]),
        2 => format!("{} and {} joined", names[0], names[1]),
        3 => format!("{}, {} and {} joined", names[0], names[1], names[2]),
        n => format!("{}, {} and {} others joined", names[0], names[1], n - 2),
    }
}

impl ChannelView {
    /// Work out what rows the transcript has, without rendering any of them.
    ///
    /// The list asks for rows by index, so the shape of the transcript — where
    /// the day dividers fall, which messages are continuations, where the
    /// unread mark sits — has to be decided up front and stay fixed until the
    /// transcript itself changes. Deciding it inside the render callback would
    /// mean walking the whole conversation to draw one visible row.
    pub(super) fn rebuild_rows(&mut self, cx: &mut App) {
        let mut rows = Vec::with_capacity(self.transcript.len() + 4);
        if self.has_more {
            rows.push(Row::LoadMore);
        }

        let mut previous: Option<&Message> = None;
        let mut divider_shown = false;

        for row in rows_of(self.transcript.entries()) {
            let Some(first) = (match &row {
                TranscriptRow::Message(entry) => Some(*entry),
                TranscriptRow::Joins(run) => run.first().copied(),
            }) else {
                continue;
            };
            let message = &first.message;

            if previous.is_none_or(|p| time::crosses_day_boundary(&p.ts, &message.ts)) {
                rows.push(Row::Day(SharedString::from(time::day_heading(&message.ts))));
                previous = None;
            }

            if let TranscriptRow::Joins(run) = &row {
                rows.push(Row::Joins(run.iter().map(|e| e.ts().clone()).collect()));
                previous = Some(&run[run.len() - 1].message);
                continue;
            }

            let unread = !divider_shown
                && self
                    .unread_from
                    .as_ref()
                    .is_some_and(|mark| message.ts.as_f64() > mark.as_f64());
            divider_shown |= unread;

            // A run of messages from one person within a few minutes reads as
            // one block; repeating the avatar and name would only add noise.
            let grouped = previous.is_some_and(|p| {
                p.author_id() == message.author_id()
                    && !p.is_system_notice()
                    && !message.is_system_notice()
                    && time::within_grouping_window(&p.ts, &message.ts)
            }) && !unread;

            rows.push(Row::Message {
                ts: message.ts.clone(),
                grouped,
                unread,
            });
            previous = Some(message);
        }

        if rows.iter().all(|row| matches!(row, Row::LoadMore)) {
            rows.push(Row::Empty);
        }

        // One selection participant per message, carrying the text a copy
        // should produce. Handles for messages that scrolled out of the window
        // are dropped with them.
        let mut selections = HashMap::with_capacity(self.transcript.len());
        for row in &rows {
            let Row::Message { ts, .. } = row else {
                continue;
            };
            let Some(entry) = self.transcript.get(ts) else {
                continue;
            };
            let text = slack_api::markup::to_plain_text(&entry.message.text);
            let handle = match self.selections.remove(ts) {
                Some(handle) => {
                    handle.set_fallback_copy_text(text, cx);
                    handle
                }
                None => gpui_base::TextSelectionHandle::new(text, cx),
            };
            selections.insert(ts.clone(), handle);
        }
        self.selections = selections;

        self.rows = rows;
        self.list.reset(self.rows.len());
    }

    /// Tell the list that one row's content changed, so it remeasures.
    pub(super) fn invalidate_row(&self, ts: &Ts) {
        let found = self.rows.iter().position(|row| match row {
            Row::Message { ts: row_ts, .. } => row_ts == ts,
            _ => false,
        });
        if let Some(index) = found {
            self.list.splice(index..index + 1, 1);
        }
    }

    /// Draw one row. Called by the list for the rows that are on screen.
    pub(super) fn render_row(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(row) = self.rows.get(index).cloned() else {
            return div().into_any_element();
        };

        match row {
            Row::LoadMore => h_flex()
                .w_full()
                .justify_center()
                .py_2()
                .child(
                    Button::new("load-older")
                        .ghost()
                        .small()
                        .label("Load earlier messages")
                        .loading(self.loading_older)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.fetch_older(window, cx)
                        })),
                )
                .into_any_element(),

            Row::Day(label) => day_divider(label, cx),

            Row::Joins(timestamps) => {
                let run: Vec<&crate::transcript::Entry> = timestamps
                    .iter()
                    .filter_map(|ts| self.transcript.get(ts))
                    .collect();
                self.render_joins(&run, cx)
            }

            Row::Empty => self.render_empty(cx),

            Row::Message {
                ts,
                grouped,
                unread,
            } => {
                // An edit replaces the row in place, so the composer appears
                // where the message was.
                if let Some(session) = &self.editing
                    && session.ts == ts
                {
                    return div()
                        .w_full()
                        .px_4()
                        .py_2()
                        .child(session.composer.clone())
                        .into_any_element();
                }

                let Some(entry) = self.transcript.get(&ts) else {
                    return div().into_any_element();
                };
                let message = entry.message.clone();
                let (me, emoji) = {
                    let store = self.store.read(cx);
                    (
                        SharedString::from(store.identity().user_id.clone()),
                        Rc::new(store.emoji().clone()),
                    )
                };
                let actions = self.message_actions(cx);

                let selection = self
                    .selections
                    .get(&ts)
                    .map(|handle| (handle.clone(), index as u64));
                self.render_message(
                    &message, grouped, unread, selection, &me, &emoji, &actions, cx,
                )
            }
        }
    }

    pub(super) fn render_transcript(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            // The cache is set before `id`, because it belongs to `Div`.
            .image_cache(self.images.clone())
            .id("transcript")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, index, window, cx| this.render_row(index, window, cx)),
                )
                .size_full(),
            )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_message(
        &self,
        message: &Message,
        grouped: bool,
        unread_here: bool,
        selection: Option<(gpui_base::TextSelectionHandle, u64)>,
        me: &SharedString,
        emoji: &Rc<EmojiIndex>,
        actions: &MessageActions,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let store = self.store.read(cx);
        let author_id = message.author_id().unwrap_or_default().to_string();
        let author = message
            .username
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| store.user_name(&author_id));
        let avatar = store
            .user(&author_id)
            .and_then(|u| u.avatar_url())
            .map(|url| SharedString::from(url.to_string()));

        let entry = self.transcript.get(&message.ts);
        let blocks = entry
            .map(|e| e.blocks.clone())
            .unwrap_or_else(|| Rc::new(Vec::new()));

        MessageRow::new(
            message.ts.clone(),
            author,
            blocks,
            emoji.clone(),
            me.clone(),
            actions.clone(),
        )
        .author_id(author_id.clone())
        .avatar(avatar)
        .reactions(message.reactions.clone())
        .files(self.thumbnails.attach(&message.files))
        .replies(
            message.reply_count.unwrap_or(0),
            message
                .reply_users
                .iter()
                .map(|u| store.user_name(u))
                .collect(),
        )
        .edited(message.edited.is_some())
        .grouped(grouped)
        .own(store.is_me(&author_id))
        .system(message.is_system_notice())
        .unread_divider(unread_here)
        .when_some(selection, |row, (handle, order)| {
            row.selection(handle, order)
        })
        .into_any_element()
    }

    /// The one line that explains why the transcript is not simply live.
    pub(super) fn notice(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let (text, tone) = match &self.state {
            LoadState::Failed(message) => (message.clone(), cx.theme().danger),
            LoadState::Stale => (
                SharedString::from("Offline — showing saved messages"),
                cx.theme().warning,
            ),
            _ => return None,
        };

        Some(
            div()
                .w_full()
                .px_4()
                .py_2()
                .bg(tone.opacity(0.1))
                .text_sm()
                .text_color(tone)
                .child(text)
                .into_any_element(),
        )
    }

    /// One quiet line for a run of joins and leaves.
    pub(super) fn render_joins(
        &self,
        run: &[&crate::transcript::Entry],
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let store = self.store.read(cx);
        let names: Vec<SharedString> = run
            .iter()
            .filter_map(|entry| entry.message.user.as_deref())
            .map(|id| store.user_name(id))
            .collect();

        // Built from the message row's own columns — gutter then body — so the
        // text lands on the same reading edge as every other line.
        h_flex()
            .w_full()
            .items_start()
            .gap_3()
            .px_4()
            .py_1()
            .child(div().w_9().flex_shrink_0())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(summarise_joins(&names))),
            )
            .into_any_element()
    }

    pub(super) fn render_empty(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (name, emoji) = {
            let store = self.store.read(cx);
            (
                self.channel
                    .as_ref()
                    .and_then(|id| store.conversation(id))
                    .map(|c| c.name.clone())
                    .unwrap_or_default(),
                store.emoji().clone(),
            )
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .py_8()
            .text_color(cx.theme().muted_foreground)
            .child(div().text_lg().child(emoji_glyph("wave", &emoji, cx)))
            .child(div().font_semibold().child(match self.state {
                LoadState::Loading => SharedString::from("Loading messages…"),
                _ => SharedString::from(format!("This is the start of {name}")),
            }))
            .when(self.state == LoadState::Ready, |this| {
                this.child(div().text_sm().child("Say something to get it going."))
            })
            .into_any_element()
    }
}
