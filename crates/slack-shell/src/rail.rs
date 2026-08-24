//! The icon rail: which navigation pane is showing.
//!
//! A fixed, narrow column outside the resizable panes, because it is the one
//! piece of navigation that must never move or change width — everything else
//! on screen is reached through it.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    ClickEvent, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    v_flex,
};

use gpui::SharedUri;
use gpui_component::avatar::Avatar;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use slack_api::models::Presence;

use slack_ui::icons::SlackIcon;
use slack_workspace::store::{WorkspaceEvent, WorkspaceStore};

/// Snooze durations offered in the account menu, in minutes.
const SNOOZE_CHOICES: &[(u32, &str)] = &[
    (30, "For 30 minutes"),
    (60, "For 1 hour"),
    (120, "For 2 hours"),
    (480, "Until tomorrow"),
];

/// Which navigation pane the rail is pointing at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Chats,
    DirectMessages,
    Activity,
}

impl Pane {
    const ALL: [Pane; 3] = [Pane::Chats, Pane::DirectMessages, Pane::Activity];

    fn label(self) -> &'static str {
        match self {
            Pane::Chats => "Chats",
            Pane::DirectMessages => "Direct messages",
            Pane::Activity => "Activity",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Pane::Chats => Icon::new(SlackIcon::Chats),
            Pane::DirectMessages => Icon::new(SlackIcon::DirectMessages),
            Pane::Activity => Icon::new(IconName::Bell),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Pane::Chats => "rail-chats",
            Pane::DirectMessages => "rail-dms",
            Pane::Activity => "rail-activity",
        }
    }
}

/// The rail's width. Fixed: a rail that resized would defeat its purpose.
pub const RAIL_WIDTH: f32 = 56.;

#[derive(Debug, Clone)]
pub enum RailEvent {
    Selected(Pane),
}

pub struct Rail {
    store: Entity<WorkspaceStore>,
    pane: Pane,
    /// Shown on the activity icon when something is waiting.
    unread: bool,
}

impl EventEmitter<RailEvent> for Rail {}

impl Rail {
    pub fn new(store: Entity<WorkspaceStore>) -> Self {
        Self {
            store,
            pane: Pane::Chats,
            unread: false,
        }
    }

    pub fn pane(&self) -> Pane {
        self.pane
    }

    pub fn set_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane == pane {
            return;
        }
        self.pane = pane;
        cx.emit(RailEvent::Selected(pane));
        cx.notify();
    }

    pub fn set_unread(&mut self, unread: bool, cx: &mut Context<Self>) {
        if self.unread == unread {
            return;
        }
        self.unread = unread;
        cx.notify();
    }
}

impl Render for Rail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .flex_shrink_0()
            .w(px(RAIL_WIDTH))
            .items_center()
            .gap_1()
            .py_2()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .gap_1()
                    .children(Pane::ALL.map(|pane| {
                        let selected = pane == self.pane;
                        let badge = self.unread && pane == Pane::Activity && !selected;

                        div()
                            .relative()
                            .child(
                                Button::new(SharedString::from(pane.id()))
                                    .ghost()
                                    .icon(pane.icon())
                                    .tooltip(pane.label())
                                    .selected(selected)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.set_pane(pane, cx)
                                    })),
                            )
                            .when(badge, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top(px(4.))
                                        .right(px(4.))
                                        .size(px(8.))
                                        .rounded_full()
                                        .bg(cx.theme().danger),
                                )
                            })
                    })),
            )
            .child(self.render_account(cx))
    }
}

impl Rail {
    /// The account control, at the foot of the rail the way a desktop chat
    /// client puts it. It carries presence, notifications, theme, and signing
    /// out — everything about *you* rather than about a conversation.
    fn render_account(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let me = store.identity().user_id.clone();
        let name = SharedString::from(store.identity().user.clone());
        let avatar_url = store
            .user(&me)
            .and_then(|user| user.avatar_url())
            .map(|url| SharedString::from(url.to_string()));
        let away = store.presence() == Presence::Away;
        let snoozing = store.dnd().snooze_enabled;
        let store = self.store.clone();

        Button::new("account")
            .ghost()
            .child(
                div()
                    .relative()
                    .child(
                        Avatar::new()
                            .name(name.clone())
                            .with_size(px(28.))
                            .when_some(avatar_url, |this, url| {
                                this.src(SharedUri::from(url.to_string()))
                            }),
                    )
                    // Presence is on the avatar, which is where a chat client
                    // is looked at for it.
                    .child(
                        div()
                            .absolute()
                            .bottom(px(-1.))
                            .right(px(-1.))
                            .size(px(10.))
                            .rounded_full()
                            .border_2()
                            .border_color(cx.theme().sidebar)
                            .bg(if away || snoozing {
                                cx.theme().muted_foreground
                            } else {
                                cx.theme().success
                            }),
                    ),
            )
            .tooltip(name)
            .dropdown_menu(move |menu, _, _| {
                let store = store.clone();

                let menu = menu
                    .item(
                        PopupMenuItem::new(if away {
                            "Set yourself as active"
                        } else {
                            "Set yourself as away"
                        })
                        .on_click({
                            let store = store.clone();
                            move |_, _, cx| {
                                let next = if away {
                                    Presence::Active
                                } else {
                                    Presence::Away
                                };
                                store.update(cx, |store, cx| store.set_presence(next, cx));
                            }
                        }),
                    )
                    .separator();

                let menu = if snoozing {
                    menu.item(PopupMenuItem::new("Resume notifications").on_click({
                        let store = store.clone();
                        move |_, _, cx| {
                            store.update(cx, |store, cx| store.snooze(None, cx));
                        }
                    }))
                } else {
                    SNOOZE_CHOICES.iter().fold(
                        menu.item(PopupMenuItem::label("Pause notifications")),
                        |menu, (minutes, label)| {
                            let store = store.clone();
                            let minutes = *minutes;
                            menu.item(PopupMenuItem::new(*label).on_click(move |_, _, cx| {
                                store.update(cx, |store, cx| store.snooze(Some(minutes), cx));
                            }))
                        },
                    )
                };

                menu.separator()
                    .item(
                        PopupMenuItem::new("Switch theme")
                            .icon(Icon::new(IconName::Moon))
                            .on_click(|_, window, cx| slack_ui::theme::toggle(window, cx)),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Sign out")
                            .icon(Icon::new(SlackIcon::SignOut))
                            .on_click({
                                let store = store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |_, cx| cx.emit(WorkspaceEvent::SignedOut));
                                }
                            }),
                    )
            })
    }
}
