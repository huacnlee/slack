//! Full-text search across the workspace.
//!
//! Results are deliberately a jumping-off point rather than a second
//! transcript: each hit shows where it came from, who wrote it, and enough
//! text to recognise it, and confirming one opens that conversation.
//!
//! `search.messages` needs a user token with `search:read`. A bot token is
//! refused by Slack, and that refusal is reported here rather than logged.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};

use slack_api::SearchMatch;
use slack_api::markup;

use slack_ui::time;
use slack_workspace::store::WorkspaceStore;

/// Hits requested per search.
const RESULT_COUNT: u32 = 30;

#[derive(Debug, Clone)]
pub enum SearchEvent {
    /// Open the conversation a hit came from.
    OpenConversation(SharedString),
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Nothing has been asked yet.
    Idle,
    Searching,
    /// A completed search and how many hits Slack reported in total.
    Done {
        total: u32,
    },
    Failed(SharedString),
}

pub struct SearchView {
    store: Entity<WorkspaceStore>,
    query: Entity<InputState>,
    results: Vec<SearchMatch>,
    state: State,
    /// Holding the task cancels the previous search when a new one starts.
    _search: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for SearchView {}

impl SearchView {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search messages"));

        let subscription = cx.subscribe(&query, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let text = state.read(cx).value().to_string();
                this.run(text, cx);
            }
        });

        Self {
            store,
            query,
            results: Vec::new(),
            state: State::Idle,
            _search: None,
            _subscriptions: vec![subscription],
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.query.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn run(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.results.clear();
            self.state = State::Idle;
            cx.notify();
            return;
        }

        self.state = State::Searching;
        cx.notify();

        let client = self.store.read(cx).client().clone();
        self._search = Some(cx.spawn(async move |this, cx| {
            let found = client.search_messages(&query, RESULT_COUNT).await;

            _ = this.update(cx, |this, cx| {
                match found {
                    Ok(results) => {
                        this.state = State::Done {
                            total: results.total,
                        };
                        this.results = results.matches;
                    }
                    Err(err) if err.is_missing_scope() => {
                        this.state = State::Failed(
                            "This token cannot search. Search needs a user token with the search:read scope.".into(),
                        );
                    }
                    Err(err) => {
                        this.state = State::Failed(format!("Search failed: {err}").into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn render_result(
        &self,
        index: usize,
        hit: &SearchMatch,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let channel = hit
            .channel
            .as_ref()
            .map(|c| SharedString::from(format!("#{}", c.name)))
            .unwrap_or_else(|| SharedString::from("Direct message"));
        let channel_id = hit
            .channel
            .as_ref()
            .map(|c| SharedString::from(c.id.clone()))
            .unwrap_or_default();

        let author = {
            let store = self.store.read(cx);
            hit.username
                .clone()
                .map(SharedString::from)
                .or_else(|| hit.user.as_ref().map(|id| store.user_name(id)))
                .unwrap_or_else(|| SharedString::from("Unknown"))
        };

        // The preview is flattened rather than fully rendered: a result list
        // is for recognising a message, not for reading it in place.
        let preview = SharedString::from(markup::to_plain_text(&hit.text));
        let when = SharedString::from(time::relative(&hit.ts));

        v_flex()
            .id(("hit", index))
            .w_full()
            .gap_1()
            .px_4()
            .py_2()
            .cursor_pointer()
            .hover(|this| this.bg(cx.theme().accent.opacity(0.4)))
            .when(!channel_id.is_empty(), |this| {
                this.on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(SearchEvent::OpenConversation(channel_id.clone()))
                }))
            })
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().font_semibold().child(channel))
                    .child(div().child(author))
                    .child(div().child(when)),
            )
            .child(div().text_sm().line_clamp(3).child(preview))
    }

    /// The one line above the results that says what just happened.
    fn render_status(&self) -> Option<SharedString> {
        match &self.state {
            State::Idle => Some("Type a query and press Enter.".into()),
            State::Searching => Some("Searching…".into()),
            State::Failed(message) => Some(message.clone()),
            State::Done { .. } if self.results.is_empty() => Some("No messages match that.".into()),
            State::Done { total } => {
                let shown = self.results.len();
                Some(SharedString::from(if *total as usize > shown {
                    format!("Showing {shown} of {total} matches")
                } else if shown == 1 {
                    "1 match".to_string()
                } else {
                    format!("{shown} matches")
                }))
            }
        }
    }
}

impl Focusable for SearchView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let failed = matches!(self.state, State::Failed(_));
        let status = self.render_status();
        let results: Vec<_> = self
            .results
            .iter()
            .enumerate()
            .map(|(ix, hit)| self.render_result(ix, hit, cx).into_any_element())
            .collect();

        v_flex()
            .w(px(640.))
            .max_h(px(520.))
            .gap_2()
            .child(
                Input::new(&self.query)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(if failed {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(status),
                )
            })
            .when(!results.is_empty(), |this| {
                this.child(
                    div()
                        .id("search-results")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(v_flex().w_full().children(results)),
                )
            })
    }
}
