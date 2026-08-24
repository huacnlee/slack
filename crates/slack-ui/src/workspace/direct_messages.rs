//! Direct messages on their own, newest first.
//!
//! The conversation tree groups direct messages with everything else and
//! orders them inside their section; this pane is only them, and it shows the
//! one thing the tree has no room for — who you last spoke to, and when.
//!
//! Recency comes from the background sweep. A conversation it has not reached
//! yet sorts by name and shows no time rather than a wrong one.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, StyledExt as _, avatar::Avatar, h_flex, v_flex,
};

use slack_api::models::{ChannelKind, Ts};

use crate::people::PersonTrigger;
use crate::time;
use crate::workspace::store::{Conversation, WorkspaceEvent, WorkspaceStore};

#[derive(Debug, Clone)]
pub enum DirectMessagesEvent {
    Open(SharedString),
}

pub struct DirectMessagesView {
    store: Entity<WorkspaceStore>,
    filter: Entity<InputState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DirectMessagesEvent> for DirectMessagesView {}

impl DirectMessagesView {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find a person"));

        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&filter, |_, _, cx| cx.notify()),
            cx.subscribe(&store, |_, _, event: &WorkspaceEvent, cx| {
                if matches!(event, WorkspaceEvent::ConversationsChanged) {
                    cx.notify();
                }
            }),
        ];

        Self {
            store,
            filter,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Direct messages, newest first.
    fn rows(&self, cx: &App) -> Vec<Conversation> {
        let needle = self.filter.read(cx).value().to_lowercase();
        let mut rows: Vec<Conversation> = self
            .store
            .read(cx)
            .listable()
            .filter(|c| c.kind.is_dm())
            .filter(|c| needle.is_empty() || c.name.to_lowercase().contains(&needle))
            .cloned()
            .collect();

        rows.sort_by(|a, b| {
            let a_ts = a.latest.as_ref().map(Ts::as_f64).unwrap_or(0.0);
            let b_ts = b.latest.as_ref().map(Ts::as_f64).unwrap_or(0.0);
            b_ts.total_cmp(&a_ts)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        rows
    }

    fn render_row(
        &self,
        index: usize,
        conversation: &Conversation,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = conversation.id.clone();
        let selected = self.store.read(cx).selected_id() == Some(&conversation.id);
        let unread = conversation.has_unread();
        let when = conversation
            .latest
            .as_ref()
            .map(|ts| SharedString::from(time::relative(ts)))
            .unwrap_or_default();

        h_flex()
            .id(("dm", index))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(cx.theme().radius)
            .when(selected, |this| {
                this.bg(cx.theme().sidebar_accent)
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
            .when(!selected, |this| {
                this.hover(|this| this.bg(cx.theme().accent.opacity(0.5)))
            })
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(DirectMessagesEvent::Open(id.clone()))
            }))
            .child(self.render_face(index, conversation, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(unread, |this| this.font_semibold())
                    .child(conversation.name.clone()),
            )
            .when(!when.is_empty(), |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(when.clone()),
                )
            })
            .when(unread, |this| {
                this.child(
                    div()
                        .size(px(8.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(cx.theme().danger),
                )
            })
    }

    /// The other person, or a group icon for a multi-person message.
    fn render_face(
        &self,
        index: usize,
        conversation: &Conversation,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        if conversation.kind == ChannelKind::Mpim {
            return Icon::new(crate::icons::SlackIcon::Users)
                .small()
                .text_color(cx.theme().muted_foreground)
                .into_any_element();
        }

        let Some(counterpart) = conversation.counterpart.clone() else {
            return Icon::new(IconName::CircleUser).small().into_any_element();
        };
        let avatar_url = self
            .store
            .read(cx)
            .user(&counterpart)
            .and_then(|user| user.avatar_url())
            .map(|url| gpui::SharedUri::from(url.to_string()));

        let avatar = Avatar::new()
            .name(conversation.name.clone())
            .with_size(px(24.))
            .when_some(avatar_url, |this, url| this.src(url));

        PersonTrigger::new(
            SharedString::from(format!("dm-face-{index}")),
            self.store.clone(),
            counterpart,
            avatar,
        )
        .into_any_element()
    }
}

impl Focusable for DirectMessagesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DirectMessagesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);

        v_flex()
            .size_full()
            .min_w_0()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_2()
                    .child(div().font_semibold().child("Direct messages"))
                    .child(
                        Input::new(&self.filter)
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    ),
            )
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No direct messages"),
                )
            })
            .when(!rows.is_empty(), |this| {
                this.child(
                    div()
                        .id("dm-list")
                        .flex_1()
                        .min_h_0()
                        .px_1()
                        .overflow_y_scrollbar()
                        .child(v_flex().w_full().gap_px().py_1().children(
                            rows.iter().enumerate().map(|(index, conversation)| {
                                self.render_row(index, conversation, cx).into_any_element()
                            }),
                        )),
                )
            })
    }
}
