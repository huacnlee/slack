//! What has happened since you last looked.
//!
//! Slack has no activity endpoint for an OAuth token, so this is assembled
//! from the two things that *are* reachable: conversations the background
//! sweep found unread, and a message search for your own handle. That is why
//! there are two filters here and not Slack's five — threads and
//! reactions-to-your-messages need internal endpoints an app cannot call, and
//! a tab that could never fill would be worse than no tab.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Task, Window, div, px,
};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    tab::{Tab, TabBar},
    v_flex,
};

use slack_api::markup;
use slack_api::models::Ts;

use crate::icons::SlackIcon;
use crate::people::PersonTrigger;
use crate::time;
use crate::workspace::store::{WorkspaceEvent, WorkspaceStore};

/// Mentions requested per refresh.
const MENTION_COUNT: u32 = 30;

/// Which slice of activity is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Mentions,
    Unreads,
}

impl Filter {
    const ALL: [Filter; 3] = [Filter::All, Filter::Mentions, Filter::Unreads];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Mentions => "Mentions",
            Filter::Unreads => "Unreads",
        }
    }
}

/// Why something is in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Mention,
    Unread,
}

/// One line of activity.
#[derive(Debug, Clone)]
pub struct Item {
    pub reason: Reason,
    pub channel: SharedString,
    /// `#general`, or a person's name for a direct message.
    pub channel_label: SharedString,
    pub author_id: SharedString,
    pub author: SharedString,
    pub ts: Ts,
    pub preview: SharedString,
}

#[derive(Debug, Clone)]
pub enum ActivityEvent {
    /// Open the conversation this line came from.
    Open(SharedString),
}

pub struct ActivityView {
    store: Entity<WorkspaceStore>,
    filter: Filter,
    mentions: Vec<Item>,
    /// Set while the mention search is in flight.
    loading: bool,
    error: Option<SharedString>,
    focus: FocusHandle,
    /// Holding the task cancels a previous search when a new one starts.
    _search: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<ActivityEvent> for ActivityView {}

impl ActivityView {
    pub fn new(store: Entity<WorkspaceStore>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&store, |_, _, event: &WorkspaceEvent, cx| {
            if matches!(event, WorkspaceEvent::ConversationsChanged) {
                cx.notify();
            }
        });

        let mut this = Self {
            store,
            filter: Filter::All,
            mentions: Vec::new(),
            loading: false,
            error: None,
            focus: cx.focus_handle(),
            _search: None,
            _subscriptions: vec![subscription],
        };
        this.refresh(cx);
        this
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        cx.notify();
    }

    /// Fetch the mentions half. The unread half is already in the store.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        cx.notify();

        let store = self.store.read(cx);
        let client = store.client().clone();
        // Searching for the handle is what finds `<@U…>` in message text;
        // Slack resolves it server side.
        let query = format!("@{}", store.identity().user);

        self._search = Some(cx.spawn(async move |this, cx| {
            let found = client.search_messages(&query, MENTION_COUNT).await;

            _ = this.update(cx, |this, cx| {
                this.loading = false;
                match found {
                    Ok(results) => {
                        this.mentions = results
                            .matches
                            .into_iter()
                            .filter_map(|hit| this.item_from_hit(hit, cx))
                            .collect();
                    }
                    Err(err) if err.is_missing_scope() => {
                        this.error =
                            Some("Mentions need a token with the search:read scope.".into());
                    }
                    Err(err) => this.error = Some(format!("Could not load mentions: {err}").into()),
                }
                cx.notify();
            });
        }));
    }

    fn item_from_hit(&self, hit: slack_api::SearchMatch, cx: &App) -> Option<Item> {
        let store = self.store.read(cx);
        let channel = hit.channel.as_ref()?;
        let channel_id = SharedString::from(channel.id.clone());
        let author_id = hit.user.clone().unwrap_or_default();

        Some(Item {
            reason: Reason::Mention,
            channel_label: self.label_for(&channel_id, &channel.name, cx),
            channel: channel_id,
            author: hit
                .username
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| store.user_name(&author_id)),
            author_id: SharedString::from(author_id),
            ts: hit.ts,
            preview: SharedString::from(markup::to_plain_text(&hit.text)),
        })
    }

    /// A channel reads as `#name`; a direct message reads as the person.
    fn label_for(&self, id: &str, fallback: &str, cx: &App) -> SharedString {
        match self.store.read(cx).conversation(id) {
            Some(conversation) if conversation.kind.is_dm() => conversation.name.clone(),
            Some(conversation) => format!("#{}", conversation.name).into(),
            None => format!("#{fallback}").into(),
        }
    }

    /// Conversations the sweep found unread, newest first.
    fn unreads(&self, cx: &App) -> Vec<Item> {
        let store = self.store.read(cx);
        let mut items: Vec<Item> = store
            .listable()
            .filter(|conversation| conversation.has_unread())
            .map(|conversation| Item {
                reason: Reason::Unread,
                channel: conversation.id.clone(),
                channel_label: if conversation.kind.is_dm() {
                    conversation.name.clone()
                } else {
                    format!("#{}", conversation.name).into()
                },
                author_id: conversation.counterpart.clone().unwrap_or_default(),
                author: conversation.name.clone(),
                ts: conversation.latest.clone().unwrap_or_default(),
                // The store keeps timestamps, not message text; the unread
                // line says where to look rather than pretending to preview.
                preview: "New messages".into(),
            })
            .collect();

        items.sort_by(|a, b| b.ts.as_f64().total_cmp(&a.ts.as_f64()));
        items
    }

    fn items(&self, cx: &App) -> Vec<Item> {
        match self.filter {
            Filter::Mentions => self.mentions.clone(),
            Filter::Unreads => self.unreads(cx),
            Filter::All => {
                let mut all = self.unreads(cx);
                all.extend(self.mentions.iter().cloned());
                all.sort_by(|a, b| b.ts.as_f64().total_cmp(&a.ts.as_f64()));
                all
            }
        }
    }

    fn render_row(&self, index: usize, item: &Item, cx: &mut Context<Self>) -> impl IntoElement {
        let channel = item.channel.clone();
        let (icon, reason) = match item.reason {
            Reason::Mention => (SlackIcon::AtSign, "Mention in"),
            Reason::Unread => (SlackIcon::Thread, "Unread in"),
        };

        v_flex()
            .id(("activity", index))
            .w_full()
            .gap_1()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .hover(|this| this.bg(cx.theme().accent.opacity(0.5)))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(ActivityEvent::Open(channel.clone()))
            }))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(self.render_author(index, item, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(item.author.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(time::relative(&item.ts))),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .pl(px(28.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(icon).xsmall())
                    .child(div().child(SharedString::from(reason)))
                    .child(div().truncate().child(item.channel_label.clone())),
            )
            .child(
                div()
                    .w_full()
                    .pl(px(28.))
                    .text_sm()
                    .line_clamp(2)
                    .child(item.preview.clone()),
            )
    }

    /// The author's avatar, carrying the same person behaviour as everywhere
    /// else. An unread row has no single author, so it shows the icon instead.
    fn render_author(&self, index: usize, item: &Item, _: &Context<Self>) -> gpui::AnyElement {
        if item.author_id.is_empty() {
            return div()
                .size(px(20.))
                .flex_shrink_0()
                .child(Icon::new(SlackIcon::Hash).xsmall())
                .into_any_element();
        }

        let avatar = gpui_component::avatar::Avatar::new()
            .name(item.author.clone())
            .with_size(px(20.));

        PersonTrigger::new(
            SharedString::from(format!("activity-avatar-{index}")),
            self.store.clone(),
            item.author_id.clone(),
            avatar,
        )
        .into_any_element()
    }
}

impl Focusable for ActivityView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ActivityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.items(cx);
        let filter = self.filter;

        v_flex()
            .size_full()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .child(
                        TabBar::new("activity-filters")
                            .segmented()
                            .selected_index(
                                Filter::ALL.iter().position(|f| *f == filter).unwrap_or(0),
                            )
                            .children(Filter::ALL.map(|option| Tab::new().label(option.label())))
                            .on_click(cx.listener(|this, index: &usize, _, cx| {
                                if let Some(option) = Filter::ALL.get(*index) {
                                    this.set_filter(*option, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("refresh-activity")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(SlackIcon::Refresh))
                            .tooltip("Check for new activity")
                            .loading(self.loading)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(error),
                )
            })
            .when(items.is_empty(), |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .p_6()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.loading {
                            "Looking for new activity…"
                        } else {
                            "Nothing new"
                        }),
                )
            })
            .when(!items.is_empty(), |this| {
                this.child(
                    div()
                        .id("activity-list")
                        .flex_1()
                        .min_h_0()
                        .px_1()
                        .overflow_y_scrollbar()
                        .child(v_flex().w_full().gap_1().py_1().children(
                            items.iter().enumerate().map(|(index, item)| {
                                self.render_row(index, item, cx).into_any_element()
                            }),
                        )),
                )
            })
    }
}
