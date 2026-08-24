//! Showing who someone is.
//!
//! One card describes a person, at two depths: a compact form for the hover
//! that appears over an avatar, a name, or a mention, and a fuller one for the
//! profile sheet a click opens. Both read from the workspace directory, so a
//! member who is not in it degrades to their id rather than to an empty card.
//!
//! [`PersonTrigger`] is the seam every one of those places goes through. An
//! avatar, a name, and a mention are three different elements with the same
//! contract — hover to see who this is, click to open their profile — and
//! giving them one component is what keeps that contract identical rather
//! than three near-copies that drift.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyView, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, SharedUri, StatefulInteractiveElement as _,
    Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, Placement, Sizable as _, StyledExt as _, WindowExt as _, avatar::Avatar,
    h_flex, hover_card::HoverCard, v_flex,
};

use slack_api::models::User;

use crate::icons::SlackIcon;
use crate::workspace::store::WorkspaceStore;

/// How much of a person to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardDepth {
    /// For a hover: enough to recognise them.
    Hover,
    /// For the profile sheet: everything the directory knows.
    Full,
}

/// A person, rendered from the workspace directory.
#[derive(IntoElement)]
pub struct UserCard {
    store: Entity<WorkspaceStore>,
    user_id: SharedString,
    depth: CardDepth,
}

impl UserCard {
    pub fn new(
        store: Entity<WorkspaceStore>,
        user_id: impl Into<SharedString>,
        depth: CardDepth,
    ) -> Self {
        Self {
            store,
            user_id: user_id.into(),
            depth,
        }
    }
}

impl gpui::RenderOnce for UserCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = self.store.read(cx);
        let user = store.user(&self.user_id).cloned();
        let emoji = store.emoji().clone();

        let Some(user) = user else {
            return unknown_person(&self.user_id, cx).into_any_element();
        };

        let avatar_size = match self.depth {
            CardDepth::Hover => px(48.),
            CardDepth::Full => px(88.),
        };
        let status = user
            .profile
            .status_text
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|text| {
                let emoji_text = user
                    .profile
                    .status_emoji
                    .as_deref()
                    .map(|e| emoji.render_unicode(e))
                    .unwrap_or_default();
                SharedString::from(format!("{emoji_text} {text}").trim().to_string())
            });

        v_flex()
            .gap_3()
            .min_w(px(240.))
            .max_w(px(320.))
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        Avatar::new()
                            .name(user.display_name().to_string())
                            .when_some(user.avatar_url(), |this, url| {
                                this.src(SharedUri::from(url.to_string()))
                            })
                            .size(avatar_size),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .font_semibold()
                                    .child(SharedString::from(user.display_name().to_string())),
                            )
                            .children(real_name_line(&user))
                            .children(user.profile.title.as_deref().filter(|t| !t.is_empty()).map(
                                |title| {
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(SharedString::from(title.to_string()))
                                },
                            )),
                    ),
            )
            .when_some(status, |this, status| {
                this.child(div().text_sm().child(status))
            })
            .when(self.depth == CardDepth::Full, |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .pt_3()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .children(
                            user.tz
                                .as_deref()
                                .filter(|tz| !tz.is_empty())
                                .map(|tz| detail_row(SlackIcon::AtSign, tz.to_string(), cx)),
                        )
                        .child(detail_row(SlackIcon::Hash, user.id.clone(), cx))
                        .when(user.is_bot, |this| {
                            this.child(detail_row(SlackIcon::Hash, "App".to_string(), cx))
                        })
                        .when(user.deleted, |this| {
                            this.child(
                                div()
                                    .text_color(cx.theme().danger)
                                    .child("This account is deactivated"),
                            )
                        }),
                )
            })
            .into_any_element()
    }
}

/// Show the real name only when it adds something the display name did not.
fn real_name_line(user: &User) -> Option<impl IntoElement> {
    let real = user
        .profile
        .real_name
        .as_deref()
        .or(user.real_name.as_deref())?;
    if real.is_empty() || real == user.display_name() {
        return None;
    }
    Some(div().text_sm().child(SharedString::from(real.to_string())))
}

fn detail_row(icon: SlackIcon, text: String, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(
            Icon::new(icon)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .child(SharedString::from(text))
        .into_any_element()
}

fn unknown_person(id: &SharedString, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .min_w(px(200.))
        .child(div().font_semibold().child(id.clone()))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Not in the workspace directory"),
        )
}

/// Anything that stands for a person: an avatar, a name, a mention.
///
/// Wrapping a child in this gives it the workspace's person behaviour without
/// the caller re-implementing hover timing, card layout, or what a click does.
#[derive(IntoElement)]
pub struct PersonTrigger {
    id: ElementId,
    store: Entity<WorkspaceStore>,
    user_id: SharedString,
    child: gpui::AnyElement,
}

impl PersonTrigger {
    pub fn new(
        id: impl Into<ElementId>,
        store: Entity<WorkspaceStore>,
        user_id: impl Into<SharedString>,
        child: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            store,
            user_id: user_id.into(),
            child: child.into_any_element(),
        }
    }
}

impl gpui::RenderOnce for PersonTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let card_store = self.store.clone();
        let card_user = self.user_id.clone();
        let click_store = self.store;
        let click_user = self.user_id;

        HoverCard::new(self.id.clone())
            .trigger(
                div()
                    .id(self.id)
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        open_profile(click_store.clone(), click_user.clone(), window, cx)
                    })
                    .child(self.child),
            )
            .content(move |_, _, _| {
                UserCard::new(card_store.clone(), card_user.clone(), CardDepth::Hover)
            })
    }
}

/// A card wrapped as a view, which is what a tooltip has to be.
pub struct UserCardView {
    store: Entity<WorkspaceStore>,
    user_id: SharedString,
}

impl UserCardView {
    /// Build the hover card for `user_id` as a tooltip view.
    pub fn tooltip(
        store: Entity<WorkspaceStore>,
        user_id: impl Into<SharedString>,
        cx: &mut App,
    ) -> AnyView {
        let user_id = user_id.into();
        cx.new(|_| Self { store, user_id }).into()
    }
}

impl Render for UserCardView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_md()
            .child(UserCard::new(
                self.store.clone(),
                self.user_id.clone(),
                CardDepth::Hover,
            ))
    }
}

/// Open the profile sheet for `user_id`.
///
/// A sheet rather than a dialog: a profile is something to consult beside the
/// conversation, not a decision that has to be dismissed before reading on.
pub fn open_profile(
    store: Entity<WorkspaceStore>,
    user_id: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    let user_id = user_id.into();
    let title = store.read(cx).user_name(&user_id);

    window.open_sheet_at(Placement::Right, cx, move |sheet, _, _| {
        let store = store.clone();
        let user_id = user_id.clone();
        sheet
            .title(title.clone())
            .size(px(360.))
            .child(
                div()
                    .p_4()
                    .child(UserCard::new(store, user_id, CardDepth::Full)),
            )
    });
}
