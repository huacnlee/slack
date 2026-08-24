//! The reply pane for one message.
//!
//! A thread is a second transcript over the same conversation, so it reuses
//! the transcript window, the message row, and the composer. What differs is
//! that every message posted here carries the root timestamp, and rows do not
//! offer to open a thread of their own.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use slack_api::models::Ts;

use crate::channel::composer::{Composer, ComposerEvent, ComposerMode};
use crate::channel::message_row::{MessageActions, MessageRow};
use crate::channel::transcript::Transcript;
use crate::time;
use crate::workspace::store::{WorkspaceEvent, WorkspaceStore};

/// Replies fetched per request. Threads are rarely longer than this, and a
/// thread that is gets its oldest replies dropped by the transcript window.
const PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone)]
pub enum ThreadEvent {
    /// The reader closed the pane.
    Closed,
    /// A reply landed, so the channel transcript's reply count is stale.
    RepliesChanged {
        root: Ts,
    },
    Failed(SharedString),
}

pub struct ThreadView {
    store: Entity<WorkspaceStore>,
    channel: SharedString,
    root: Ts,
    transcript: Transcript,
    loading: bool,
    error: Option<SharedString>,
    composer: Entity<Composer>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ThreadEvent> for ThreadView {}

impl ThreadView {
    pub fn new(
        store: Entity<WorkspaceStore>,
        channel: SharedString,
        root: Ts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| Composer::new("Reply…", ComposerMode::Compose, window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.subscribe_in(&store, window, |this, _, event, _, cx| {
                if matches!(event, WorkspaceEvent::ActivityPolled) {
                    this.load(cx);
                }
            }),
        ];

        let mut this = Self {
            store,
            channel,
            root,
            transcript: Transcript::default(),
            loading: true,
            error: None,
            composer,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this
    }

    pub fn root(&self) -> &Ts {
        &self.root
    }

    pub fn channel(&self) -> &SharedString {
        &self.channel
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let client = self.store.read(cx).client().clone();
        let channel = self.channel.clone();
        let root = self.root.clone();
        self.loading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let page = client
                .conversation_replies(&channel, &root, PAGE_SIZE)
                .await;

            _ = this.update(cx, |this, cx| {
                this.loading = false;
                // A reply that arrives for a thread the reader has left is
                // not for this pane.
                if this.channel != channel || this.root != root {
                    return;
                }
                match page {
                    Ok(page) => {
                        this.error = None;
                        this.transcript.replace(page.messages);
                    }
                    Err(err) => this.error = Some(err.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send(&mut self, text: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.store.read(cx).client().clone();
        let channel = self.channel.clone();
        let root = self.root.clone();

        cx.spawn_in(window, async move |this, cx| {
            let result = client.post_message(&channel, &text, Some(&root)).await;

            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    this.composer
                        .update(cx, |composer, cx| composer.accept(window, cx));
                    cx.emit(ThreadEvent::RepliesChanged { root: root.clone() });
                    this.load(cx);
                }
                Err(err) => {
                    this.composer.update(cx, |composer, cx| composer.reject(cx));
                    cx.emit(ThreadEvent::Failed(
                        format!("Could not post that reply: {err}").into(),
                    ));
                }
            });
        })
        .detach();
    }

    fn toggle_reaction(&mut self, ts: Ts, name: SharedString, cx: &mut Context<Self>) {
        let client = self.store.read(cx).client().clone();
        let me = self.store.read(cx).identity().user_id.clone();
        let channel = self.channel.clone();

        let Some(entry) = self.transcript.get(&ts) else {
            return;
        };
        let mine = entry
            .message
            .reactions
            .iter()
            .any(|r| r.name == name.as_ref() && r.users.contains(&me));

        cx.spawn(async move |this, cx| {
            let result = if mine {
                client.remove_reaction(&channel, &ts, &name).await
            } else {
                client.add_reaction(&channel, &ts, &name).await
            };
            _ = this.update(cx, |this, cx| {
                if let Err(err) = result
                    && !matches!(err.slack_code(), Some("already_reacted" | "no_reaction"))
                {
                    cx.emit(ThreadEvent::Failed(
                        format!("Could not change that reaction: {err}").into(),
                    ));
                    return;
                }
                this.load(cx);
            });
        })
        .detach();
    }

    fn on_composer_event(
        &mut self,
        _: &Entity<Composer>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let ComposerEvent::Submit(text) = event {
            self.send(text.clone(), window, cx);
        }
    }

    fn message_actions(&self, cx: &Context<Self>) -> MessageActions {
        MessageActions {
            toggle_reaction: Rc::new(cx.listener(
                |this, (ts, name): &(Ts, SharedString), _, cx| {
                    this.toggle_reaction(ts.clone(), name.clone(), cx)
                },
            )),
            // Rows inside a thread never offer to open one.
            open_thread: Rc::new(|_, _, _| {}),
            start_edit: Rc::new(|_, _, _| {}),
            delete: Rc::new(|_, _, _| {}),
            copy_link: Rc::new(cx.listener(|this, ts: &Ts, _, cx| {
                let client = this.store.read(cx).client().clone();
                let channel = this.channel.clone();
                let ts = ts.clone();
                cx.spawn(async move |this, cx| {
                    if let Ok(link) = client.message_permalink(&channel, &ts).await {
                        _ = this.update(cx, |_, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(link))
                        });
                    }
                })
                .detach();
            })),
            open_file: Rc::new(|url: &SharedString, _: &mut Window, cx: &mut App| cx.open_url(url)),
            follow_link: {
                let store = self.store.clone();
                Rc::new(move |link, window, cx| match link {
                    slack_api::markup::Link::Url(url) => cx.open_url(url),
                    slack_api::markup::Link::User(id) => {
                        crate::people::open_profile(store.clone(), id.clone(), window, cx)
                    }
                    _ => {}
                })
            },
            resolve_name: {
                let store = self.store.clone();
                Rc::new(move |link, cx: &App| {
                    crate::channel::channel_view::resolve_link_label(link, &store, cx)
                })
            },
            hover_link: {
                let store = self.store.clone();
                Rc::new(move |link, _, cx: &mut App| match link {
                    slack_api::markup::Link::User(id) => Some(
                        crate::people::UserCardView::tooltip(store.clone(), id.clone(), cx),
                    ),
                    _ => None,
                })
            },
            open_profile: {
                let store = self.store.clone();
                Rc::new(
                    move |id: &SharedString, window: &mut Window, cx: &mut App| {
                        crate::people::open_profile(store.clone(), id.clone(), window, cx)
                    },
                )
            },
            store: self.store.clone(),
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = {
            let store = self.store.read(cx);
            store
                .conversation(&self.channel)
                .map(|c| c.name.clone())
                .unwrap_or_default()
        };

        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .min_w_0()
                    .child(div().font_semibold().child("Thread"))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(name),
                    ),
            )
            .child(
                Button::new("close-thread")
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::Close))
                    .tooltip("Close thread")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::Closed))),
            )
    }
}

impl Focusable for ThreadView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (me, emoji) = {
            let store = self.store.read(cx);
            (
                SharedString::from(store.identity().user_id.clone()),
                Rc::new(store.emoji().clone()),
            )
        };
        let actions = self.message_actions(cx);
        let entries = self.transcript.entries();

        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(entries.len());
        let mut previous: Option<&slack_api::models::Message> = None;

        for (ix, entry) in entries.iter().enumerate() {
            let message = &entry.message;
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
            let own = store.is_me(&author_id);

            // The root is always its own block; only replies group.
            let grouped = ix > 0
                && previous.is_some_and(|p| {
                    p.author_id() == message.author_id()
                        && time::within_grouping_window(&p.ts, &message.ts)
                });

            rows.push(
                MessageRow::new(
                    message.ts.clone(),
                    author,
                    entry.blocks.clone(),
                    emoji.clone(),
                    me.clone(),
                    actions.clone(),
                )
                .author_id(author_id.clone())
                .avatar(avatar)
                .reactions(message.reactions.clone())
                .files(Vec::new())
                .edited(message.edited.is_some())
                .grouped(grouped)
                .own(own)
                .system(message.is_system_notice())
                .threadable(false)
                .into_any_element(),
            );

            // A separator after the root marks where the replies begin.
            if ix == 0 && entries.len() > 1 {
                rows.push(
                    div()
                        .w_full()
                        .px_4()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(match entries.len() - 1 {
                            1 => "1 reply".to_string(),
                            n => format!("{n} replies"),
                        }))
                        .into_any_element(),
                );
            }
            previous = Some(message);
        }

        v_flex()
            .size_full()
            .min_w_0()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .when_some(self.error.clone(), |this, message| {
                this.child(
                    div()
                        .w_full()
                        .px_4()
                        .py_2()
                        .bg(cx.theme().danger.opacity(0.1))
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
            .child(
                div()
                    .id("thread-transcript")
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&self.scroll)
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .py_2()
                            .when(self.loading && rows.is_empty(), |this| {
                                this.items_center().py_8().child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Loading replies…"),
                                )
                            })
                            .children(rows),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .p_3()
                    .child(self.composer.clone()),
            )
    }
}
