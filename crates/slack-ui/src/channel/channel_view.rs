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

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, list, px,
};
use gpui_component::{
    ActiveTheme, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use slack_api::emoji::EmojiIndex;
use slack_api::models::{ChannelKind, Message, Ts};

use crate::channel::attachments::{self, Thumbnails};
use crate::channel::composer::{Composer, ComposerEvent, ComposerMode};
use crate::channel::markup_view::{HoverLink, OnLink, ResolveName};
use crate::channel::message_row::{MessageActions, MessageRow, day_divider, emoji_glyph};
use crate::channel::transcript::{Row as TranscriptRow, Transcript, rows as rows_of};
use crate::icons::SlackIcon;
use crate::time;
use crate::workspace::store::{WorkspaceEvent, WorkspaceStore};
use slack_api::markup::Link;

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

/// "Ada joined", "Ada and Bob joined", "Ada, Bob and 5 others joined".
///
/// Naming the first two keeps the line useful — you usually care that someone
/// specific arrived — while the count keeps it one line however many did.
fn summarise_joins(names: &[SharedString]) -> String {
    match names.len() {
        0 => "Someone joined the channel".to_string(),
        1 => format!("{} joined", names[0]),
        2 => format!("{} and {} joined", names[0], names[1]),
        3 => format!("{}, {} and {} joined", names[0], names[1], names[2]),
        n => format!("{}, {} and {} others joined", names[0], names[1], n - 2),
    }
}

/// Fill in the label Slack omitted from a `<@U…>` or `<#C…>` escape.
///
/// Returning `None` leaves the parser's own fallback in place, which is the
/// bare id — better than an empty span when the directory does not have them.
pub fn resolve_link_label(
    link: &Link,
    store: &Entity<crate::workspace::store::WorkspaceStore>,
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

/// One row of the rendered transcript, as the list addresses them.
///
/// Timestamps rather than messages: the list asks for a row long after the
/// shape was decided, and the message it names may have been edited since.
#[derive(Debug, Clone)]
enum Row {
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
    images: Entity<crate::images::LruImageCache>,
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
            images: crate::images::LruImageCache::new(crate::images::DEFAULT_CAPACITY, cx),
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
        self.rebuild_rows();

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

    fn fetch_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                        this.rebuild_rows();
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

    fn fetch_older(&mut self, _: &mut Window, cx: &mut Context<Self>) {
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
                        this.rebuild_rows();
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
    fn fetch_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                this.rebuild_rows();
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

    // ------------------------------------------------------------ commands

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

    fn save_edit(
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

    fn confirm_delete(&mut self, ts: Ts, window: &mut Window, cx: &mut Context<Self>) {
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

    fn delete(&mut self, channel: SharedString, ts: Ts, cx: &mut Context<Self>) {
        let client = self.store.read(cx).client().clone();
        // Remove it locally first: the row is gone from the reader's view the
        // moment they confirm, and a failed call puts it back.
        self.transcript.remove(&ts);
        self.rebuild_rows();
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

    fn toggle_reaction(
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

    fn copy_link(&mut self, ts: Ts, cx: &mut Context<Self>) {
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

    fn start_edit(&mut self, ts: Ts, window: &mut Window, cx: &mut Context<Self>) {
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
    fn attach_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Pull down the image attachments on screen.
    ///
    /// Slack serves thumbnails from a host that requires the token, so they
    /// are fetched with the API client and cached rather than handed to the
    /// image loader as URLs.
    fn fetch_thumbnails(&mut self, cx: &mut Context<Self>) {
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
    fn resolve_unknown_authors(&mut self, cx: &mut Context<Self>) {
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
    fn cache_key(channel: &str) -> String {
        format!("messages/{channel}")
    }

    /// Remember the tail of the transcript for the next launch.
    fn persist(&self, cx: &Context<Self>) {
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

    fn mark_read(&mut self, cx: &mut Context<Self>) {
        let (Some(channel), Some(ts)) = (self.channel.clone(), self.transcript.last_ts().cloned())
        else {
            return;
        };
        self.store
            .update(cx, |store, cx| store.mark_read(channel, ts, cx));
    }

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
            open_file: Rc::new(|url: &SharedString, _: &mut Window, cx: &mut App| cx.open_url(url)),
            follow_link: self.link_handler(cx),
            resolve_name: self.name_resolver(cx),
            hover_link: self.link_hover(cx),
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

    /// Fills in the labels Slack left out of a message body.
    fn name_resolver(&self, _: &Context<Self>) -> ResolveName {
        let store = self.store.clone();
        Rc::new(move |link, cx: &App| resolve_link_label(link, &store, cx))
    }

    /// The card shown when the pointer rests on a mention.
    fn link_hover(&self, _: &Context<Self>) -> HoverLink {
        let store = self.store.clone();
        Rc::new(move |link, _, cx: &mut App| match link {
            Link::User(id) => Some(crate::people::UserCardView::tooltip(
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
            Link::User(id) => crate::people::open_profile(store.clone(), id.clone(), window, cx),
            // A broadcast names a group, which this client has no view of.
            Link::Broadcast(_) => {}
        })
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

    /// Work out what rows the transcript has, without rendering any of them.
    ///
    /// The list asks for rows by index, so the shape of the transcript — where
    /// the day dividers fall, which messages are continuations, where the
    /// unread mark sits — has to be decided up front and stay fixed until the
    /// transcript itself changes. Deciding it inside the render callback would
    /// mean walking the whole conversation to draw one visible row.
    fn rebuild_rows(&mut self) {
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

        self.rows = rows;
        self.list.reset(self.rows.len());
    }

    /// Tell the list that one row's content changed, so it remeasures.
    fn invalidate_row(&self, ts: &Ts) {
        let found = self.rows.iter().position(|row| match row {
            Row::Message { ts: row_ts, .. } => row_ts == ts,
            _ => false,
        });
        if let Some(index) = found {
            self.list.splice(index..index + 1, 1);
        }
    }

    /// Draw one row. Called by the list for the rows that are on screen.
    fn render_row(
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
                let run: Vec<&crate::channel::transcript::Entry> = timestamps
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

                self.render_message(&message, grouped, unread, &me, &emoji, &actions, cx)
            }
        }
    }

    fn render_transcript(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
    fn render_message(
        &self,
        message: &Message,
        grouped: bool,
        unread_here: bool,
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
        .into_any_element()
    }

    /// The one line that explains why the transcript is not simply live.
    fn notice(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
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
    fn render_joins(
        &self,
        run: &[&crate::channel::transcript::Entry],
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

    fn render_empty(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .p_3()
                    .child(self.composer.clone()),
            )
            .into_any_element()
    }
}
