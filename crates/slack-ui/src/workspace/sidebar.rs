//! Workspace navigation: who you are, and where you can go.
//!
//! The list is a [`Tree`] of three collapsible sections — starred, channels,
//! direct messages — filtered by one field at the top. Tree rather than a flat
//! menu for two reasons: this workspace has a thousand conversations, and the
//! tree virtualizes its rows; and sections that a reader can collapse are the
//! only way a list that long stays navigable.
//!
//! Selection is written straight to the shared store, because the sidebar is
//! not the owner of which conversation is open; it only asks for a change.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::list::ListItem;
use gpui_component::menu::PopupMenuItem;
use gpui_component::sidebar::SidebarHeader;
use gpui_component::tree::{Tree, TreeItem, TreeState, tree};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use slack_api::models::{ChannelKind, Presence, Ts};

use crate::icons::SlackIcon;
use crate::time;
use crate::workspace::store::{Connectivity, Conversation, Section, WorkspaceStore};

#[derive(Debug, Clone)]
pub enum SidebarEvent {
    /// The reader asked to sign out of this workspace.
    SignOutRequested,
}

pub struct SidebarView {
    store: Entity<WorkspaceStore>,
    filter: Entity<InputState>,
    tree: Entity<TreeState>,
    /// What the tree currently shows, so a rebuild that would change nothing
    /// can be skipped.
    signature: Vec<(SharedString, SharedString)>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter conversations"));
        let tree = cx.new(|cx| TreeState::new(cx));

        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| {
                this.rebuild(cx);
                this.sync_selection(cx);
                cx.notify();
            }),
            cx.observe(&filter, |this, _, cx| {
                this.rebuild(cx);
                cx.notify();
            }),
        ];

        let mut this = Self {
            store,
            filter,
            tree,
            signature: Vec::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.rebuild(cx);
        this
    }

    pub fn focus_filter(&self, window: &mut Window, cx: &mut App) {
        let handle = self.filter.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// The conversations to show, already grouped, filtered, and ordered.
    fn sections(&self, cx: &App) -> Vec<(Section, Vec<Conversation>)> {
        let needle = self.filter.read(cx).value().to_lowercase();
        let store = self.store.read(cx);

        Section::ALL
            .into_iter()
            .map(|section| {
                let mut rows = store
                    // `listable` drops direct messages that turned out to be
                    // empty; Slack keeps one for everyone you have ever opened
                    // a DM with, so without this half the list is dead rows.
                    .listable()
                    .filter(|c| c.section() == section)
                    .filter(|c| needle.is_empty() || c.name.to_lowercase().contains(&needle))
                    .cloned()
                    .collect::<Vec<_>>();
                order(section, &mut rows);
                (section, rows)
            })
            .collect()
    }

    /// Rebuild the tree from the store, but only when the rows actually
    /// changed.
    ///
    /// The store notifies every few seconds while the background sweep learns
    /// about the workspace. Handing `set_items` a fresh list each time resets
    /// the tree — including its scroll position — so the list would jump back
    /// to the top under the reader's pointer. The signature is what the tree
    /// would render; if that is unchanged there is nothing to rebuild.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let sections: Vec<(Section, Vec<Conversation>)> = self
            .sections(cx)
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .collect();

        let signature = signature(&sections);
        if signature == self.signature {
            return;
        }
        self.signature = signature;

        let items: Vec<TreeItem> = sections
            .into_iter()
            .map(|(section, rows)| {
                TreeItem::new(section.id(), section.label())
                    .expanded(true)
                    .children(rows.into_iter().map(|conversation| {
                        TreeItem::new(conversation.id.clone(), row_label(&conversation))
                    }))
            })
            .collect();

        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        self.sync_selection(cx);
    }

    /// Keep the tree's own cursor on the conversation the store has open, so
    /// keyboard navigation starts from what is on screen.
    fn sync_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.store.read(cx).selected_id().cloned() else {
            return;
        };
        let index = self.tree.read(cx).index_of(&selected);
        self.tree.update(cx, |tree, cx| {
            tree.set_selected_index(index, cx);
        });
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> Tree {
        let store = self.store.clone();
        let menu_store = self.store.clone();
        let selected = self.store.read(cx).selected_id().cloned();

        tree(&self.tree, move |ix, entry, _, _, cx| {
            let item = entry.item();
            let id = item.id.clone();

            if entry.is_folder() {
                // A section reads as a header, but it is a control: the
                // chevron is what says the rows below can be put away.
                return ListItem::new(ix).w_full().child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_1()
                        .px_1()
                        .py_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            Icon::new(if entry.is_expanded() {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall(),
                        )
                        .child(item.label.clone()),
                );
            }

            let conversation = store.read(cx).conversation(&id).cloned();
            let Some(conversation) = conversation else {
                return ListItem::new(ix).w_full().child(item.label.clone());
            };

            let unread = conversation.unread;
            let is_selected = selected.as_ref() == Some(&id);
            let click_store = store.clone();
            let click_id = id.clone();
            let star_store = store.clone();
            let star_id = id.clone();
            let starred = conversation.starred;
            let group = SharedString::from(format!("row-{id}"));

            // Selection is styled here rather than through `ListItem`'s own
            // selected state: that state paints the same accent as hover, and
            // a navigation list has to tell "where I am" from "where the
            // pointer is".
            ListItem::new(ix)
                .w_full()
                .on_click(move |_, _, cx| {
                    click_store.update(cx, |store, cx| store.select(click_id.clone(), cx));
                })
                .child(
                    h_flex()
                        .group(group.clone())
                        .w_full()
                        .gap_2()
                        .items_center()
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .when(is_selected, |this| {
                            this.bg(cx.theme().sidebar_accent)
                                .text_color(cx.theme().sidebar_accent_foreground)
                                .font_medium()
                        })
                        .child(
                            Icon::new(SlackIcon::for_channel(conversation.kind))
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                // Unread is carried by weight as well as the
                                // dot: colour alone is not a signal.
                                .when(unread > 0, |this| this.font_semibold())
                                .child(item.label.clone()),
                        )
                        .child(unread_pill(unread, cx))
                        .child(
                            // Quiet at rest. The same command is on the row's
                            // context menu, so hover is not its only route.
                            div()
                                .when(!starred, |this| {
                                    this.invisible()
                                        .group_hover(group.clone(), |this| this.visible())
                                })
                                .child(
                                    Button::new(("star", ix))
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(if starred {
                                            IconName::StarFill
                                        } else {
                                            IconName::Star
                                        }))
                                        .tooltip(if starred { "Unstar" } else { "Star" })
                                        .on_click(move |_: &ClickEvent, _, cx| {
                                            star_store.update(cx, |store, cx| {
                                                store.toggle_star(&star_id, cx)
                                            });
                                        }),
                                ),
                        ),
                )
        })
        .context_menu(move |_, entry, menu, _, cx| {
            let item = entry.item();
            if entry.is_folder() {
                return menu;
            }
            let starred = menu_store
                .read(cx)
                .conversation(&item.id)
                .is_some_and(|c| c.starred);
            let store = menu_store.clone();
            let id = item.id.clone();

            menu.item(
                PopupMenuItem::new(if starred {
                    "Remove from Starred"
                } else {
                    "Add to Starred"
                })
                .icon(Icon::new(if starred {
                    IconName::StarOff
                } else {
                    IconName::Star
                }))
                .on_click(move |_, _, cx| {
                    store.update(cx, |store, cx| store.toggle_star(&id, cx));
                }),
            )
        })
    }

    fn render_header(&self, cx: &mut Context<Self>) -> SidebarHeader {
        let team = SharedString::from(self.store.read(cx).identity().team.clone());

        SidebarHeader::new()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_8()
                    .flex_shrink_0()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().sidebar_primary)
                    .text_color(cx.theme().sidebar_primary_foreground)
                    .font_semibold()
                    .child(SharedString::from(
                        team.chars()
                            .next()
                            .unwrap_or('S')
                            .to_uppercase()
                            .to_string(),
                    )),
            )
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .child(div().font_semibold().truncate().child(team))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.status_line(cx)),
                    ),
            )
    }

    fn status_line(&self, cx: &App) -> SharedString {
        let store = self.store.read(cx);
        if store.connectivity() == Connectivity::Offline {
            return "Offline".into();
        }
        let dnd = store.dnd();
        if dnd.snooze_enabled {
            return SharedString::from(format!(
                "Notifications paused until {}",
                time::until_clock(dnd.snooze_endtime)
            ));
        }
        match store.presence() {
            Presence::Active => "Active".into(),
            Presence::Away => "Away".into(),
        }
    }
}

impl Focusable for SidebarView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.store.read(cx).listable().next().is_none();

        // Composed from the sidebar's theme tokens rather than the `Sidebar`
        // container: that container's contract is groups of menu items, and
        // the tree replaced that model to get collapsible sections and
        // virtualized rows.
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
                    .child(self.render_header(cx))
                    .child(
                        Input::new(&self.filter)
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .when(empty, |this| {
                        this.child(
                            div()
                                .p_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No conversations"),
                        )
                    })
                    .when(!empty, |this| this.child(self.render_tree(cx))),
            )
    }
}

/// Order one section's rows.
///
/// Channels are alphabetical because that is how they are looked for. Direct
/// messages are by recency because that is how they are remembered; within the
/// same recency, and for the ones whose timestamp is not yet known, by name.
fn order(section: Section, rows: &mut [Conversation]) {
    match section {
        Section::DirectMessages => rows.sort_by(|a, b| {
            let a_ts = a.latest.as_ref().map(Ts::as_f64).unwrap_or(0.0);
            let b_ts = b.latest.as_ref().map(Ts::as_f64).unwrap_or(0.0);
            b_ts.total_cmp(&a_ts)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }),
        Section::Starred | Section::Channels => rows.sort_by_key(|c| c.name.to_lowercase()),
    }
}

/// What the tree would render: every row's identity and label, in order.
///
/// Unread is not part of it — that changes the weight of a row, not the shape
/// of the list, and rebuilding for it would defeat the point.
fn signature(sections: &[(Section, Vec<Conversation>)]) -> Vec<(SharedString, SharedString)> {
    sections
        .iter()
        .flat_map(|(section, rows)| {
            std::iter::once((SharedString::from(section.id()), SharedString::from("")))
                .chain(rows.iter().map(|c| (c.id.clone(), row_label(c))))
        })
        .collect()
}

/// The label for one conversation row.
fn row_label(conversation: &Conversation) -> SharedString {
    match conversation.kind {
        ChannelKind::Public | ChannelKind::Private => {
            SharedString::from(format!("#{}", conversation.name))
        }
        _ => conversation.name.clone(),
    }
}

/// The trailing unread marker on a conversation row.
///
/// Slack gives an OAuth token no unread *count*, only enough to know whether
/// anything is newer than the read marker, so this is a dot rather than a
/// number it would have to invent.
fn unread_pill(unread: u32, cx: &App) -> gpui::AnyElement {
    if unread == 0 {
        return div().into_any_element();
    }

    div()
        .size(px(8.))
        .flex_shrink_0()
        .rounded_full()
        .bg(cx.theme().danger)
        .into_any_element()
}
