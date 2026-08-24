//! Sending a message on to another conversation.
//!
//! Slack forwards by posting the message's permalink: its own server expands
//! that back into the original, quoted, with its author and channel intact.
//! Re-typing the text into a new message would look similar and be a lie —
//! it would carry this reader's name, lose the thread it came from, and go
//! stale the moment the original was edited.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme, Icon, WindowExt as _, h_flex, v_flex};

use slack_api::models::{ChannelKind, Ts};
use slack_ui::icons::SlackIcon;
use slack_workspace::store::{Conversation, WorkspaceStore};

/// Enough to choose from without turning the dialog into a directory.
const MAX_RESULTS: usize = 8;

pub struct ForwardView {
    store: Entity<WorkspaceStore>,
    /// The message being forwarded, and where it lives now.
    source: SharedString,
    ts: Ts,
    query: Entity<InputState>,
    note: Entity<InputState>,
    matches: Vec<Conversation>,
    /// Which match Enter would send to.
    highlighted: usize,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ForwardView {
    pub fn new(
        store: Entity<WorkspaceStore>,
        source: SharedString,
        ts: Ts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search for a conversation"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Add a note (optional)"));

        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event, window, cx| match event {
                InputEvent::Change => this.search(cx),
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&note, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.confirm(window, cx);
                }
            }),
        ];

        let mut this = Self {
            store,
            source,
            ts,
            query,
            note,
            matches: Vec::new(),
            highlighted: 0,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.search(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.query.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        let store = self.store.read(cx);
        self.matches = slack_workspace::store::matching(store.listable(), &query, MAX_RESULTS);
        self.highlighted = 0;
        cx.notify();
    }

    /// Post to whatever is highlighted, if anything is.
    ///
    /// The dialog does the sending rather than reporting a choice upwards, so
    /// every surface that can show a message — the transcript, a thread —
    /// forwards the same way without carrying its own copy of how.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.matches.get(self.highlighted).cloned() else {
            return;
        };
        let note = self.note.read(cx).value().trim().to_string();
        let client = self.store.read(cx).client().clone();
        let (source, ts, name) = (self.source.clone(), self.ts.clone(), target.name.clone());

        cx.spawn_in(window, async move |_, cx| {
            let result = async {
                let link = client.message_permalink(&source, &ts).await?;
                // The note leads, the way a covering line does; the quote
                // Slack builds from the link follows it.
                let body = if note.is_empty() {
                    link
                } else {
                    format!("{note}\n{link}")
                };
                client.post_message(&target.id, &body, None).await
            }
            .await;

            _ = cx.update(|window, cx| match result {
                Ok(_) => {
                    window.push_notification(SharedString::from(format!("Forwarded to {name}")), cx)
                }
                Err(err) => window.push_notification(
                    gpui_component::notification::Notification::error(SharedString::from(format!(
                        "Could not forward that message: {err}"
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Open the dialog for one message.
    pub fn open(
        store: Entity<WorkspaceStore>,
        source: SharedString,
        ts: Ts,
        window: &mut Window,
        cx: &mut App,
    ) {
        let picker = cx.new(|cx| ForwardView::new(store, source, ts, window, cx));

        window.open_dialog(cx, move |dialog, _, _| {
            let picker = picker.clone();
            let confirming = picker.clone();

            dialog
                .title("Forward message")
                .w(px(420.))
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default()
                        .ok_text("Forward")
                        .on_ok(move |_, window, cx| {
                            let ready = confirming.read(cx).has_target();
                            confirming.update(cx, |picker, cx| picker.confirm(window, cx));
                            // Staying open is the honest answer when there is
                            // nowhere to send it yet.
                            ready
                        }),
                )
                .content(move |content, window, cx| {
                    let to_focus = picker.clone();
                    window.defer(cx, move |window, cx| {
                        to_focus.update(cx, |picker, cx| picker.focus(window, cx));
                    });
                    content.child(picker.clone())
                })
        });
    }

    /// Whether there is anything to send to — the dialog's OK button is only
    /// meaningful when there is.
    pub fn has_target(&self) -> bool {
        self.highlighted < self.matches.len()
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.highlighted = match delta {
            d if d < 0 => self.highlighted.checked_sub(1).unwrap_or(last),
            _ if self.highlighted >= last => 0,
            _ => self.highlighted + 1,
        };
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => self.move_highlight(-1, cx),
            "down" => self.move_highlight(1, cx),
            _ => {}
        }
    }

    fn render_match(
        &self,
        index: usize,
        conversation: &Conversation,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let selected = index == self.highlighted;
        let label = match conversation.kind {
            ChannelKind::Public | ChannelKind::Private => {
                SharedString::from(format!("#{}", conversation.name))
            }
            _ => conversation.name.clone(),
        };

        h_flex()
            .id(SharedString::from(format!("forward-{}", conversation.id)))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .text_sm()
            // Hover and selection are told apart on purpose: the pointer is
            // showing what *would* happen, the highlight what will.
            .hover(|this| this.bg(cx.theme().muted))
            .when(selected, |this| {
                this.bg(cx.theme().accent)
                    .text_color(cx.theme().accent_foreground)
            })
            .child(Icon::new(SlackIcon::for_channel(conversation.kind)).size_4())
            .child(div().flex_1().min_w_0().truncate().child(label))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.highlighted = index;
                this.confirm(window, cx);
            }))
    }
}

impl Focusable for ForwardView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ForwardView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let matches: Vec<_> = self
            .matches
            .iter()
            .enumerate()
            .map(|(index, conversation)| self.render_match(index, conversation, cx))
            .collect();
        let empty = matches.is_empty();

        v_flex()
            .w_full()
            .gap_3()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(Input::new(&self.query).cleanable(true))
            .child(
                v_flex()
                    .id("forward-matches")
                    .w_full()
                    .h(px(240.))
                    .gap_0p5()
                    .overflow_y_scrollbar()
                    .when(empty, |this| {
                        this.items_center()
                            .justify_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("No conversation matches that.")
                    })
                    .children(matches),
            )
            .child(Input::new(&self.note))
    }
}
