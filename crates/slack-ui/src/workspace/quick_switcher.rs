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

use crate::icons::SlackIcon;
use crate::workspace::store::{Conversation, WorkspaceStore};

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
}

impl EventEmitter<QuickSwitcherEvent> for QuickSwitcher {}

impl QuickSwitcher {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let mut this = Self {
            store,
            state,
            matches: Vec::new(),
        };
        this.search("", cx);
        this
    }

    /// Give the palette keyboard focus once it is on screen.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.state.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_query("", window, cx));
        self.search("", cx);
    }

    fn search(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim().to_lowercase();
        let store = self.store.read(cx);

        let mut ranked: Vec<(u8, &Conversation)> = store
            .conversations()
            .iter()
            .filter_map(|conversation| {
                let name = conversation.name.to_lowercase();
                if query.is_empty() {
                    // With no query, the switcher is a recents list.
                    return Some((1, conversation));
                }
                if name.starts_with(&query) {
                    Some((0, conversation))
                } else if name.contains(&query) {
                    Some((1, conversation))
                } else {
                    None
                }
            })
            .collect();

        // The store already sorts by unread and recency; this only promotes
        // prefix matches above substring ones without disturbing that.
        ranked.sort_by_key(|(rank, _)| *rank);
        self.matches = ranked
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, conversation)| conversation.clone())
            .collect();

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
            .placeholder("Jump to a conversation")
            .min_h(gpui::px(320.))
            .max_h(gpui::px(320.))
            .items(items)
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
