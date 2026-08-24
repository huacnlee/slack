//! Jump to a conversation by typing its name.
//!
//! Filtering happens here rather than inside the palette so the confirmed row
//! index always maps back to a real conversation, and so the ranking can put
//! prefix matches above substring ones the way a switcher should.

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div,
};
use gpui_component::command::{Command, CommandItem, CommandState};
use gpui_component::{ActiveTheme, Icon, IndexPath, v_flex};

use slack_api::models::ChannelKind;

use slack_ui::icons::SlackIcon;
use slack_workspace::store::{Conversation, WorkspaceStore};

/// Rows shown at once. Enough to scan, few enough to stay one glance.
const MAX_RESULTS: usize = 12;

#[derive(Debug, Clone)]
pub enum QuickSwitcherEvent {
    /// A conversation was chosen.
    Confirmed(SharedString),
    /// The switcher was dismissed without choosing.
    Cancelled,
}

pub struct QuickSwitcher {
    store: Entity<WorkspaceStore>,
    state: Entity<CommandState>,
    /// Current results, in the order they are rendered.
    matches: Vec<Conversation>,
    /// Recently visited conversations, most recent first. With no query typed
    /// these *are* the results — an empty palette that listed the workspace
    /// alphabetically would answer a question nobody asked.
    recent: Vec<SharedString>,
}

impl EventEmitter<QuickSwitcherEvent> for QuickSwitcher {}

impl QuickSwitcher {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let mut this = Self {
            store,
            state,
            matches: Vec::new(),
            recent: Vec::new(),
        };
        this.search("", cx);
        this
    }

    /// Give the palette keyboard focus once it is on screen.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.state.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Tell the palette where the reader has been.
    pub fn set_recent(&mut self, recent: Vec<SharedString>, cx: &mut Context<Self>) {
        self.recent = recent;
        self.search("", cx);
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_query("", window, cx));
        self.search("", cx);
    }

    fn search(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim().to_lowercase();
        let store = self.store.read(cx);

        if query.is_empty() {
            // No query: this is the recents list.
            self.matches = self
                .recent
                .iter()
                .filter_map(|id| store.conversation(id).cloned())
                .take(MAX_RESULTS)
                .collect();
            cx.notify();
            return;
        }

        self.matches = slack_workspace::store::matching(store.listable(), &query, MAX_RESULTS);
        cx.notify();
    }

    fn confirm(&mut self, index: IndexPath, cx: &mut Context<Self>) {
        if index.section != 0 {
            return;
        }
        let Some(conversation) = self.matches.get(index.row) else {
            return;
        };
        cx.emit(QuickSwitcherEvent::Confirmed(conversation.id.clone()));
    }
}

impl Focusable for QuickSwitcher {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

impl Render for QuickSwitcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let confirm_owner = view.clone();
        let cancel_owner = view.clone();

        let items = self.matches.iter().map(|conversation| {
            let label = match conversation.kind {
                ChannelKind::Public | ChannelKind::Private => {
                    SharedString::from(format!("#{}", conversation.name))
                }
                _ => conversation.name.clone(),
            };
            CommandItem::new()
                .label(label)
                .icon(Icon::new(SlackIcon::for_channel(conversation.kind)))
                .keywords([conversation.name.clone()])
        });

        Command::new(&self.state)
            .bordered(false)
            .filterable(false)
            .min_h(gpui::px(320.))
            .max_h(gpui::px(320.))
            .items(items)
            .placeholder("Jump to a conversation")
            .empty(|_, _, cx| {
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_6()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(gpui_component::IconName::Search).size_8())
                    .child(div().child("No conversation matches that."))
            })
            .on_query(move |query, _, cx| {
                let query = query.to_string();
                _ = view.update(cx, |this, cx| this.search(&query, cx));
            })
            .on_confirm(move |index, _, cx| {
                _ = confirm_owner.update(cx, |this, cx| this.confirm(index, cx));
            })
            .on_cancel(move |_, cx| {
                _ = cancel_owner.update(cx, |_, cx| cx.emit(QuickSwitcherEvent::Cancelled));
            })
    }
}
