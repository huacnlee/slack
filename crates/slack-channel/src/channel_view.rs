//! The transcript and composer for one conversation.
//!
//! The view owns what is loaded, what is being written, and where the reader
//! is looking. Everything about the workspace itself — who the user is, which
//! conversations exist, what the names are — is read from the shared store.
//!
//! Two rules keep the pane honest under polling and slow networks:
//! a reply that arrives for a conversation the reader has already left is
//! dropped rather than applied, and the transcript only jumps to the newest
//! message when the reader was already reading the newest message.
//!
//! Like the workspace, the transcript is cache first. Opening a conversation
//! paints the messages that were on screen last time before any request is
//! made, and a fetch that fails leaves them there rather than replacing a
//! readable conversation with an error.

mod commands;
mod loading;
mod realtime;
mod rows;

use rows::Row;

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, list, px, rems,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use slack_api::emoji::EmojiIndex;
use slack_api::models::{ChannelKind, Message, Ts};

use crate::attachments::{self, Thumbnails};
use crate::composer::{Composer, ComposerEvent, ComposerMode};
use crate::markup_view::{HoverLink, OnLink, ResolveName};
use crate::message_row::{MessageActions, MessageRow, day_divider, emoji_glyph};
use crate::transcript::{Row as TranscriptRow, Transcript, rows as rows_of};
use slack_api::markup::Link;
use slack_ui::icons::SlackIcon;
use slack_ui::time;
use slack_workspace::store::{WorkspaceEvent, WorkspaceStore};

/// Messages fetched for the first screen of a conversation.
const PAGE_SIZE: u32 = 50;
/// Messages fetched when the reader asks for earlier history.
const OLDER_PAGE_SIZE: u32 = 50;
/// Messages kept on disk per conversation. Enough to open into a readable
/// screen offline without turning the cache into a full archive.
const CACHED_MESSAGES: usize = 60;
/// Directory lookups per loaded page, so a transcript full of former members
/// cannot turn one screen into a burst of requests.
const MAX_AUTHOR_LOOKUPS: usize = 12;
/// How far beyond the viewport the list measures rows, so scrolling does not
/// pop. A few screens of a dense transcript.
const LIST_OVERDRAW: f32 = 2048.;

/// Fill in the label Slack omitted from a `<@U…>` or `<#C…>` escape.
///
/// Returning `None` leaves the parser's own fallback in place, which is the
/// bare id — better than an empty span when the directory does not have them.
pub fn resolve_link_label(
    link: &Link,
    store: &Entity<slack_workspace::store::WorkspaceStore>,
    cx: &App,
) -> Option<SharedString> {
    let store = store.read(cx);
    match link {
        Link::User(id) => Some(format!("@{}", store.user_name(id)).into()),
        Link::Channel(id) => store
            .conversation(id)
            .map(|conversation| format!("#{}", conversation.name).into()),
        _ => None,
    }
}

/// What the pane asks the shell to do.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// Show the thread rooted at this message.
    OpenThread { channel: SharedString, root: Ts },
    /// Jump to another conversation, from a `<#C…>` mention.
    OpenConversation(SharedString),
    /// Something the reader should be told.
    Failed(SharedString),
}

/// An in-progress edit of one message.
///
/// The subscription lives here so it is dropped when the edit ends, rather
/// than accumulating one listener per message ever edited.
struct EditSession {
    ts: Ts,
    composer: Entity<Composer>,
    _subscription: Subscription,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoadState {
    Empty,
    /// Nothing on screen yet and a request in flight.
    Loading,
    Ready,
    /// Showing cached messages because the network could not be reached.
    Stale,
    /// Nothing cached and the request failed.
    Failed(SharedString),
}

pub struct ChannelView {
    store: Entity<WorkspaceStore>,
    channel: Option<SharedString>,
    transcript: Transcript,
    state: LoadState,
    has_more: bool,
    loading_older: bool,
    /// The read marker as it stood when the conversation was opened, so the
    /// "New" divider stays put instead of sliding away as rows are read.
    unread_from: Option<Ts>,
    composer: Entity<Composer>,
    /// The message being rewritten, if any.
    editing: Option<EditSession>,
    /// The file currently being shared, which blocks a second upload.
    uploading: Option<SharedString>,
    /// Local copies of the image attachments on screen.
    thumbnails: Thumbnails,
    /// Bounds what the decoded avatars and thumbnails on screen cost.
    images: Entity<slack_ui::images::LruImageCache>,
    /// One selection participant per message, so a drag across the transcript
    /// copies the messages it covered, in order.
    selections: HashMap<Ts, gpui_base::TextSelectionHandle>,
    /// The virtualized transcript. `Bottom` alignment is what makes it read
    /// like a chat log: it anchors at the newest message and grows upward.
    list: ListState,
    /// What the list draws, by index. Rebuilt whenever the transcript changes.
    rows: Vec<Row>,
    focus: FocusHandle,
    /// Bumped on every conversation switch; replies carrying an older
    /// revision are stale and discarded.
    revision: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChannelEvent> for ChannelView {}

impl ChannelView {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer =
            cx.new(|cx| Composer::new("Write a message", ComposerMode::Compose, window, cx));

        let mut subscriptions = vec![
            cx.subscribe_in(&store, window, Self::on_workspace_event),
            cx.subscribe_in(&composer, window, Self::on_composer_event),
        ];
        subscriptions.push(cx.observe(&store, |_, _, cx| cx.notify()));

        let mut this = Self {
            store,
            channel: None,
            transcript: Transcript::default(),
            state: LoadState::Empty,
            has_more: false,
            loading_older: false,
            unread_from: None,
            composer,
            editing: None,
            uploading: None,
            thumbnails: Thumbnails::default(),
            selections: HashMap::new(),
            images: slack_ui::images::LruImageCache::new(slack_ui::images::DEFAULT_CAPACITY, cx),
            list: {
                let list = ListState::new(0, ListAlignment::Bottom, px(LIST_OVERDRAW));
                // New messages scroll into view while the reader is at the
                // bottom, and stop doing so the moment they scroll up.
                list.set_follow_mode(gpui::FollowMode::Tail);
                list
            },
            rows: Vec::new(),
            focus: cx.focus_handle(),
            revision: 0,
            _subscriptions: subscriptions,
        };

        // The store may already have chosen a conversation before this view
        // existed; adopt it rather than waiting for the next change.
        if let Some(id) = this.store.read(cx).selected_id().cloned() {
            this.open(id, window, cx);
        }
        this
    }

    pub fn channel(&self) -> Option<&SharedString> {
        self.channel.as_ref()
    }

    /// The message bodies currently on screen, oldest first.
    ///
    /// Reading the rendered transcript is how a caller confirms that a send
    /// actually came back from Slack rather than merely being dispatched.
    pub fn message_texts(&self) -> Vec<String> {
        self.transcript
            .entries()
            .iter()
            .map(|entry| entry.message.text.clone())
            .collect()
    }

    /// Reload the newest page, discarding what is on screen.
    ///
    /// Used when something outside this pane changed the transcript — a reply
    /// posted in a thread, for instance, which changes a parent's reply count.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fetch_latest(window, cx);
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
    }

    // ------------------------------------------------------------ loading

    fn open(&mut self, channel: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if self.channel.as_ref() == Some(&channel) {
            return;
        }

        // Preserve what was being written before leaving.
        if let Some(previous) = self.channel.clone() {
            let draft = self.composer.read(cx).text(cx).to_string();
            self.store
                .update(cx, |store, _| store.set_draft(previous, draft));
        }

        self.revision = self.revision.wrapping_add(1);
        self.channel = Some(channel.clone());
        self.transcript.clear();
        self.editing = None;
        self.has_more = false;

        // Paint what was on screen last time before asking the network for
        // anything; on a cold start this is the difference between a readable
        // conversation and a blank pane.
        let cached: Option<Vec<Message>> =
            self.store.read(cx).cache().read(&Self::cache_key(&channel));
        self.state = match cached {
            Some(messages) if !messages.is_empty() => {
                self.transcript.replace(messages);
                // A conversation opens at its newest message, the same as it
                // would after a fetch; otherwise the cached paint lands at the
                // top and then jumps.
                self.scroll_to_tail();
                LoadState::Ready
            }
            _ => LoadState::Loading,
        };
        self.rebuild_rows(cx);

        let store = self.store.read(cx);
        self.unread_from = store
            .conversation(&channel)
            .filter(|c| c.has_unread())
            .map(|c| c.last_read.clone());
        let draft = store.draft(&channel).cloned().unwrap_or_default();
        let placeholder = store
            .conversation(&channel)
            .map(|c| match c.kind {
                ChannelKind::Im | ChannelKind::Mpim => format!("Message {}", c.name),
                _ => format!("Message #{}", c.name),
            })
            .unwrap_or_else(|| "Write a message".to_string());

        self.composer.update(cx, |composer, cx| {
            composer.set_placeholder(placeholder, window, cx);
            composer.set_text(&draft, window, cx);
        });

        cx.notify();
        self.fetch_latest(window, cx);
    }

    // ------------------------------------------------------------ commands

    // ------------------------------------------------------------ scrolling

    /// Jump to the newest message.
    ///
    /// The list follows the tail on its own while the reader is at the bottom;
    /// this is for the moments that should override where they are — opening a
    /// conversation, and sending.
    fn scroll_to_tail(&self) {
        self.list.scroll_to_end();
    }

    // ------------------------------------------------------------ events

    fn on_workspace_event(
        &mut self,
        _: &Entity<WorkspaceStore>,
        event: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspaceEvent::SelectionChanged => {
                if let Some(id) = self.store.read(cx).selected_id().cloned() {
                    self.open(id, window, cx);
                }
            }
            WorkspaceEvent::ActivityPolled => self.fetch_new(window, cx),
            WorkspaceEvent::Realtime(event) => self.apply_realtime(event, window, cx),
            WorkspaceEvent::DirectoryChanged => cx.notify(),
            _ => {}
        }
    }

    fn on_composer_event(
        &mut self,
        _: &Entity<Composer>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::Submit(text) => self.send(text.clone(), window, cx),
            ComposerEvent::Changed(text) => {
                if let Some(channel) = self.channel.clone() {
                    let text = text.to_string();
                    self.store
                        .update(cx, |store, _| store.set_draft(channel, text));
                }
            }
            ComposerEvent::Attach => self.attach_file(window, cx),
            ComposerEvent::Cancel => {}
        }
    }

    // ------------------------------------------------------------ rendering

    fn message_actions(&self, cx: &Context<Self>) -> MessageActions {
        let channel = self.channel.clone().unwrap_or_default();

        MessageActions {
            toggle_reaction: Rc::new(cx.listener(
                |this, (ts, name): &(Ts, SharedString), window, cx| {
                    this.toggle_reaction(ts.clone(), name.clone(), window, cx)
                },
            )),
            open_thread: Rc::new(cx.listener(move |_, ts: &Ts, _, cx| {
                cx.emit(ChannelEvent::OpenThread {
                    channel: channel.clone(),
                    root: ts.clone(),
                })
            })),
            start_edit: Rc::new(
                cx.listener(|this, ts: &Ts, window, cx| this.start_edit(ts.clone(), window, cx)),
            ),
            delete: Rc::new(
                cx.listener(|this, ts: &Ts, window, cx| {
                    this.confirm_delete(ts.clone(), window, cx)
                }),
            ),
            copy_link: Rc::new(cx.listener(|this, ts: &Ts, _, cx| this.copy_link(ts.clone(), cx))),
            forward: Rc::new(
                cx.listener(|this, ts: &Ts, window, cx| this.forward(ts.clone(), window, cx)),
            ),
            open_file: Rc::new(|url: &SharedString, _: &mut Window, cx: &mut App| cx.open_url(url)),
            follow_link: self.link_handler(cx),
            resolve_name: self.name_resolver(cx),
            hover_link: self.link_hover(cx),
            open_profile: {
                let store = self.store.clone();
                Rc::new(
                    move |id: &SharedString, window: &mut Window, cx: &mut App| {
                        slack_people::open_profile(store.clone(), id.clone(), window, cx)
                    },
                )
            },
            store: self.store.clone(),
        }
    }

    /// Fills in the labels Slack left out of a message body.
    fn name_resolver(&self, _: &Context<Self>) -> ResolveName {
        let store = self.store.clone();
        Rc::new(move |link, cx: &App| resolve_link_label(link, &store, cx))
    }

    /// The card shown when the pointer rests on a mention.
    fn link_hover(&self, _: &Context<Self>) -> HoverLink {
        let store = self.store.clone();
        Rc::new(move |link, _, cx: &mut App| match link {
            Link::User(id) => Some(slack_people::UserCardView::tooltip(
                store.clone(),
                id.clone(),
                cx,
            )),
            _ => None,
        })
    }

    fn link_handler(&self, cx: &Context<Self>) -> OnLink {
        let view = cx.entity().downgrade();
        let store = self.store.clone();
        Rc::new(move |link, window, cx| match link {
            Link::Url(url) => cx.open_url(url),
            Link::Channel(id) => {
                _ = view.update(cx, |_, cx| {
                    cx.emit(ChannelEvent::OpenConversation(id.clone().into()))
                });
            }
            Link::User(id) => slack_people::open_profile(store.clone(), id.clone(), window, cx),
            // A broadcast names a group, which this client has no view of.
            Link::Broadcast(_) => {}
        })
    }

    /// Who is typing, in the line above the composer.
    ///
    /// The strip is always present, even when nobody is. Letting it appear and
    /// disappear would shove the composer and the last line of the transcript
    /// down and up again every few seconds — the reader is trying to read.
    fn render_typing(&self, cx: &Context<Self>) -> impl IntoElement {
        let names = self
            .channel
            .as_ref()
            .map(|channel| self.store.read(cx).typing(channel))
            .unwrap_or_default();

        let line = match names.as_slice() {
            [] => SharedString::default(),
            [one] => format!("{one} is typing…").into(),
            [one, two] => format!("{one} and {two} are typing…").into(),
            _ => SharedString::from("Several people are typing…"),
        };

        h_flex()
            .w_full()
            .flex_shrink_0()
            .h(rems(1.125))
            .px_4()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(line)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, name, topic, is_dm, unread) = {
            let store = self.store.read(cx);
            match self.channel.as_ref().and_then(|id| store.conversation(id)) {
                Some(conversation) => (
                    SlackIcon::for_channel(conversation.kind),
                    conversation.name.clone(),
                    conversation.topic.clone(),
                    conversation.kind.is_dm(),
                    conversation.unread,
                ),
                None => (
                    SlackIcon::Hash,
                    SharedString::default(),
                    SharedString::default(),
                    false,
                    0,
                ),
            }
        };
        let _ = unread;

        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                // Identity first and at one size, so switching conversations
                // does not move the name.
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .when(!is_dm, |this| {
                        this.child(
                            Icon::new(icon)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                    })
                    .child(div().font_semibold().child(name)),
            )
            .when(!topic.is_empty(), |this| {
                this.child(
                    // The topic is context, not a heading: it truncates rather
                    // than pushing the name around.
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_3()
                        .child(div().h(px(14.)).w_px().bg(cx.theme().border))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(topic),
                        ),
                )
            })
    }
}

impl Focusable for ChannelView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChannelView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.channel.is_none() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Pick a conversation to start reading.")
                .into_any_element();
        }

        v_flex()
            .size_full()
            .min_w_0()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .when_some(self.notice(cx), |this, notice| this.child(notice))
            .child(self.render_transcript(cx))
            .when_some(self.uploading.clone(), |this, name| {
                this.child(
                    div()
                        .w_full()
                        .px_4()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!("Sharing {name}…"))),
                )
            })
            .child(self.render_typing(cx))
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .p_3()
                    .pt_0()
                    .child(self.composer.clone()),
            )
            .into_any_element()
    }
}
