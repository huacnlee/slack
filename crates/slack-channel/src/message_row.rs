//! One message in a transcript.
//!
//! The row is value-like: every input is supplied by the owning view, and the
//! commands it offers are callbacks rather than local state. That lets the
//! channel pane and the thread pane render identical rows while each keeps its
//! own selection, editing, and scroll behavior.

use std::rc::Rc;

use gpui::HitboxBehavior;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, SharedString, SharedUri, StatefulInteractiveElement as _, Styled, Window, div, img,
    px,
};
use gpui_base::{ElementExt as _, TextSelectionHandle, TextSelectionRegistration};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::attachments::Attachment;
use gpui::{ImageSource, Resource};
use slack_api::emoji::{Emoji, EmojiIndex, FREQUENT_REACTIONS};
use slack_api::markup::Block;
use slack_api::models::{Reaction, Ts};

use crate::markup_view::{HoverLink, MessageBody, OnLink, ResolveName};
use gpui::Entity;
use slack_people::PersonTrigger;
use slack_ui::icons::SlackIcon;
use slack_ui::time;
use slack_workspace::store::WorkspaceStore;

/// A command about one message, identified by its timestamp.
pub type OnMessage = Rc<dyn Fn(&Ts, &mut Window, &mut App)>;
/// A command about one reaction on one message.
pub type OnReaction = Rc<dyn Fn(&(Ts, SharedString), &mut Window, &mut App)>;
/// A command about one attachment, identified by its permalink.
pub type OnFile = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
/// A command about one person, identified by their Slack id.
pub type OnPerson = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The commands a message row can request from its owner.
#[derive(Clone)]
pub struct MessageActions {
    /// Add or remove one reaction, named without colons.
    pub toggle_reaction: OnReaction,
    pub open_thread: OnMessage,
    pub start_edit: OnMessage,
    pub delete: OnMessage,
    pub copy_link: OnMessage,
    pub open_file: OnFile,
    pub follow_link: OnLink,
    /// Turns a mentioned id into a name.
    pub resolve_name: ResolveName,
    /// The card shown when the pointer rests on a mention.
    pub hover_link: HoverLink,
    /// Open someone's profile.
    pub open_profile: OnPerson,
    /// The directory, for the cards over an avatar and a name.
    pub store: Entity<WorkspaceStore>,
}

/// A message, ready to render.
#[derive(IntoElement)]
pub struct MessageRow {
    ts: Ts,
    author: SharedString,
    /// Who wrote it, for the person card over the avatar and the name.
    author_id: SharedString,
    avatar: Option<SharedString>,
    blocks: Rc<Vec<Block>>,
    emoji: Rc<EmojiIndex>,
    reactions: Vec<Reaction>,
    files: Vec<Attachment>,
    reply_count: u32,
    repliers: Vec<SharedString>,
    edited: bool,
    /// A continuation of the message above: no avatar, no repeated name.
    grouped: bool,
    /// Written by the signed-in user, which is what gates edit and delete.
    own: bool,
    /// A join/leave/topic notice, rendered as a quiet line.
    system: bool,
    /// Marks the first message the reader has not seen.
    unread_divider: bool,
    /// Threads cannot be opened from inside a thread.
    threadable: bool,
    me: SharedString,
    /// Lets this message take part in a window-wide text selection.
    selection: Option<(TextSelectionHandle, u64)>,
    actions: MessageActions,
}

impl MessageRow {
    pub fn new(
        ts: Ts,
        author: impl Into<SharedString>,
        blocks: Rc<Vec<Block>>,
        emoji: Rc<EmojiIndex>,
        me: impl Into<SharedString>,
        actions: MessageActions,
    ) -> Self {
        Self {
            ts,
            author: author.into(),
            author_id: SharedString::default(),
            avatar: None,
            blocks,
            emoji,
            reactions: Vec::new(),
            files: Vec::new(),
            reply_count: 0,
            repliers: Vec::new(),
            edited: false,
            grouped: false,
            own: false,
            system: false,
            unread_divider: false,
            threadable: true,
            me: me.into(),
            selection: None,
            actions,
        }
    }

    /// Join the window's text selection at `order` in reading order.
    ///
    /// The row registers its area rather than its glyphs, so a drag selects
    /// whole messages and copying yields their text in order. Character-level
    /// selection inside one message would mean replacing `InteractiveText`,
    /// which is what makes a mention clickable and hoverable.
    pub fn selection(mut self, handle: TextSelectionHandle, order: u64) -> Self {
        self.selection = Some((handle, order));
        self
    }

    pub fn avatar(mut self, url: Option<SharedString>) -> Self {
        self.avatar = url;
        self
    }

    /// The author's Slack id, which turns the avatar and name into a person.
    pub fn author_id(mut self, id: impl Into<SharedString>) -> Self {
        self.author_id = id.into();
        self
    }

    pub fn reactions(mut self, reactions: Vec<Reaction>) -> Self {
        self.reactions = reactions;
        self
    }

    pub fn files(mut self, files: Vec<Attachment>) -> Self {
        self.files = files;
        self
    }

    pub fn replies(mut self, count: u32, repliers: Vec<SharedString>) -> Self {
        self.reply_count = count;
        self.repliers = repliers;
        self
    }

    pub fn edited(mut self, edited: bool) -> Self {
        self.edited = edited;
        self
    }

    pub fn grouped(mut self, grouped: bool) -> Self {
        self.grouped = grouped;
        self
    }

    pub fn own(mut self, own: bool) -> Self {
        self.own = own;
        self
    }

    pub fn system(mut self, system: bool) -> Self {
        self.system = system;
        self
    }

    pub fn unread_divider(mut self, show: bool) -> Self {
        self.unread_divider = show;
        self
    }

    pub fn threadable(mut self, threadable: bool) -> Self {
        self.threadable = threadable;
        self
    }

    /// The unambiguous timestamp, for a tooltip over the short one.
    fn timestamp_tooltip(&self) -> SharedString {
        SharedString::from(time::full(&self.ts))
    }

    fn element_id(&self, part: &'static str) -> ElementId {
        (SharedString::from(format!("{part}-{}", self.ts)), 0).into()
    }
}

/// The avatar column, in rem so it scales with the interface.
///
/// The gutter and the avatar are the same measurement on purpose: the default
/// `Size::Medium` avatar is 48px, which overflows a 36px column and eats the
/// gap to the text instead of leaving one.
const GUTTER_REMS: f32 = 2.25;

impl RenderOnce for MessageRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let group = SharedString::from(format!("msg-{}", self.ts));
        let clock = time::clock(&self.ts);
        let gutter = window.rem_size() * GUTTER_REMS;

        let selection = self.selection.clone();

        v_flex()
            .w_full()
            .when(self.unread_divider, |this| this.child(unread_divider(cx)))
            .when_some(selection, |this, (handle, order)| {
                this.on_prepaint(move |bounds, window, cx| {
                    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                    handle.register(
                        TextSelectionRegistration::new(hitbox, bounds)
                            .with_document_order(order)
                            .with_text_bounds(vec![bounds]),
                        window,
                        cx,
                    );
                })
            })
            .child(
                h_flex()
                    .id(self.element_id("row"))
                    .group(group.clone())
                    .w_full()
                    .items_start()
                    .gap_3()
                    .px_4()
                    .py_1()
                    .relative()
                    .child(self.render_gutter(&clock, gutter, cx))
                    .child(self.render_content(&clock, cx))
                    .child(self.render_hover_actions(&group, cx)),
            )
    }
}

impl MessageRow {
    /// The avatar column, which becomes a hover-only timestamp for a
    /// continuation so grouped messages keep one reading column.
    fn render_gutter(&self, clock: &str, size: gpui::Pixels, cx: &App) -> AnyElement {
        if self.grouped || self.system {
            return div()
                .id(self.element_id("gutter-clock"))
                .w(size)
                .flex_shrink_0()
                .pt(px(2.))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .invisible()
                .group_hover(format!("msg-{}", self.ts), |this| this.visible())
                .tooltip({
                    let full = self.timestamp_tooltip();
                    move |window, cx| Tooltip::new(full.clone()).build(window, cx)
                })
                .child(SharedString::from(clock.to_string()))
                .into_any_element();
        }

        let avatar = Avatar::new()
            .name(self.author.clone())
            .with_size(size)
            .when_some(self.avatar.clone(), |this, url| {
                this.src(SharedUri::from(url.to_string()))
            });

        div()
            .w(size)
            .flex_shrink_0()
            .child(self.as_person(("avatar", self.ts.clone()), avatar))
            .into_any_element()
    }

    /// Wrap `child` so it behaves like every other reference to this person.
    ///
    /// Falls through unwrapped for a bot or a webhook, which has no profile to
    /// open and would otherwise offer a card that says nothing.
    fn as_person(&self, id: (&'static str, Ts), child: impl IntoElement) -> AnyElement {
        if self.author_id.is_empty() {
            return child.into_any_element();
        }
        PersonTrigger::new(
            SharedString::from(format!("{}-{}", id.0, id.1)),
            self.actions.store.clone(),
            self.author_id.clone(),
            child,
        )
        .into_any_element()
    }

    fn render_content(&self, clock: &str, cx: &App) -> AnyElement {
        if self.system {
            return div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    MessageBody::new(
                        format!("sys-{}", self.ts),
                        self.blocks.clone(),
                        self.emoji.clone(),
                    )
                    .resolve_name(self.actions.resolve_name.clone()),
                )
                .into_any_element();
        }

        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .when(!self.grouped, |this| {
                this.child(
                    h_flex()
                        .items_baseline()
                        .gap_2()
                        .child(div().font_semibold().text_sm().child(self.author.clone()))
                        .child(
                            div()
                                .id(self.element_id("clock"))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .tooltip({
                                    let full = self.timestamp_tooltip();
                                    move |window, cx| Tooltip::new(full.clone()).build(window, cx)
                                })
                                .child(SharedString::from(clock.to_string())),
                        ),
                )
            })
            .child(
                div().text_sm().child(
                    MessageBody::new(
                        format!("body-{}", self.ts),
                        self.blocks.clone(),
                        self.emoji.clone(),
                    )
                    .on_link(self.actions.follow_link.clone())
                    .resolve_name(self.actions.resolve_name.clone())
                    .hover_link(self.actions.hover_link.clone()),
                ),
            )
            .when(self.edited, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("edited"),
                )
            })
            .children(self.render_files(cx))
            .when(!self.reactions.is_empty(), |this| {
                this.child(self.render_reactions(cx))
            })
            .when(self.threadable && self.reply_count > 0, |this| {
                this.child(self.render_thread_summary(cx))
            })
            .into_any_element()
    }

    fn render_files(&self, cx: &App) -> Vec<AnyElement> {
        self.files
            .iter()
            .map(|attachment| {
                let file = &attachment.file;
                let name = SharedString::from(file.display_name().to_string());
                let permalink = SharedString::from(file.permalink.clone().unwrap_or_default());
                let open = self.actions.open_file.clone();

                v_flex()
                    .w_full()
                    .items_start()
                    .gap_1()
                    .child(
                        // `items_start` keeps the button at its own width on
                        // the body's reading edge; a stretched child would
                        // centre its label across the pane.
                        Button::new(SharedString::from(format!("file-{}", file.id)))
                            .ghost()
                            .small()
                            .icon(Icon::new(SlackIcon::FileText))
                            .label(name.clone())
                            .disabled(permalink.is_empty())
                            .on_click(move |_: &ClickEvent, window, cx| {
                                open(&permalink, window, cx)
                            }),
                    )
                    // Shown from the local copy: Slack's own thumbnail URLs
                    // need the token, which the image loader cannot send.
                    .when_some(attachment.thumbnail.clone(), |this, path| {
                        let is_video = file.is_video();
                        this.child(
                            div()
                                .relative()
                                .child(
                                    img(ImageSource::Resource(Resource::Path(path)))
                                        .max_w(px(360.))
                                        .rounded(cx.theme().radius),
                                )
                                // A video's thumbnail is a still frame, and a
                                // still frame that cannot be told from a photo
                                // is a broken image as far as a reader knows.
                                .when(is_video, |this| {
                                    this.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .size(px(44.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded_full()
                                                    .bg(cx.theme().background.opacity(0.75))
                                                    .child(
                                                        Icon::new(IconName::Play)
                                                            .text_color(cx.theme().foreground),
                                                    ),
                                            ),
                                    )
                                }),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn render_reactions(&self, cx: &App) -> AnyElement {
        let mut row = h_flex().flex_wrap().gap_1().pt_1();

        for reaction in &self.reactions {
            let name = SharedString::from(reaction.name.clone());
            let mine = reaction.users.iter().any(|u| *u == self.me);
            let toggle = self.actions.toggle_reaction.clone();
            let ts = self.ts.clone();
            let for_click = name.clone();

            row = row.child(
                Button::new(SharedString::from(format!("react-{}-{}", self.ts, name)))
                    .xsmall()
                    .outline()
                    .selected(mine)
                    .tooltip(format!(":{name}:"))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(emoji_glyph(&name, &self.emoji, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .child(SharedString::from(reaction.count.to_string())),
                            ),
                    )
                    .on_click(move |_: &ClickEvent, window, cx| {
                        toggle(&(ts.clone(), for_click.clone()), window, cx)
                    }),
            );
        }

        row.child(self.render_reaction_picker(cx))
            .into_any_element()
    }

    fn render_reaction_picker(&self, cx: &App) -> AnyElement {
        let ts = self.ts.clone();
        let toggle = self.actions.toggle_reaction.clone();
        let emoji = self.emoji.clone();

        Popover::new(self.element_id("react"))
            .trigger(
                Button::new(self.element_id("react-trigger"))
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(SlackIcon::SmilePlus))
                    .tooltip("Add reaction"),
            )
            .child(h_flex().flex_wrap().gap_1().max_w(px(220.)).children(
                FREQUENT_REACTIONS.iter().map(|name| {
                    let name = SharedString::from(*name);
                    let ts = ts.clone();
                    let toggle = toggle.clone();
                    let for_click = name.clone();
                    Button::new(SharedString::from(format!("pick-{ts}-{name}")))
                        .ghost()
                        .small()
                        .tooltip(format!(":{name}:"))
                        .child(emoji_glyph(&name, &emoji, cx))
                        .on_click(move |_: &ClickEvent, window, cx| {
                            toggle(&(ts.clone(), for_click.clone()), window, cx)
                        })
                }),
            ))
            .into_any_element()
    }

    fn render_thread_summary(&self, cx: &App) -> AnyElement {
        let ts = self.ts.clone();
        let open = self.actions.open_thread.clone();
        let label = match self.reply_count {
            1 => "1 reply".to_string(),
            n => format!("{n} replies"),
        };

        h_flex()
            .gap_2()
            .pt_1()
            .child(
                Button::new(self.element_id("thread"))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(SlackIcon::CornerDownRight))
                    .label(label)
                    .text_color(cx.theme().link)
                    .on_click(move |_: &ClickEvent, window, cx| open(&ts, window, cx)),
            )
            .into_any_element()
    }

    /// Commands that appear over the row on hover, in Slack's own order:
    /// react, reply, then everything else behind one menu.
    fn render_hover_actions(&self, group: &SharedString, cx: &App) -> AnyElement {
        if self.system {
            return div().into_any_element();
        }

        let ts = self.ts.clone();
        let copy = self.actions.copy_link.clone();
        let edit = self.actions.start_edit.clone();
        let delete = self.actions.delete.clone();
        let open_thread = self.actions.open_thread.clone();
        let own = self.own;

        h_flex()
            .absolute()
            .right_4()
            .top_0()
            .gap_1()
            .p_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .invisible()
            .group_hover(group.clone(), |this| this.visible())
            .when(self.threadable, |this| {
                let ts = ts.clone();
                this.child(
                    Button::new(self.element_id("reply"))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(SlackIcon::Thread))
                        .tooltip("Reply in thread")
                        .on_click(move |_: &ClickEvent, window, cx| open_thread(&ts, window, cx)),
                )
            })
            .child(
                Button::new(self.element_id("more"))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(gpui_component::IconName::Ellipsis))
                    .tooltip("More actions")
                    .dropdown_menu(move |menu, _, _| {
                        let copy = copy.clone();
                        let edit = edit.clone();
                        let delete = delete.clone();
                        let ts = ts.clone();

                        let menu = menu.item(
                            PopupMenuItem::new("Copy link")
                                .icon(Icon::new(SlackIcon::Link))
                                .on_click({
                                    let ts = ts.clone();
                                    move |_, window, cx| copy(&ts, window, cx)
                                }),
                        );

                        if !own {
                            return menu;
                        }

                        menu.separator()
                            .item(
                                PopupMenuItem::new("Edit message")
                                    .icon(Icon::new(SlackIcon::Pencil))
                                    .on_click({
                                        let ts = ts.clone();
                                        move |_, window, cx| edit(&ts, window, cx)
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Delete message")
                                    .icon(Icon::new(SlackIcon::Trash))
                                    .on_click(move |_, window, cx| delete(&ts, window, cx)),
                            )
                    }),
            )
            .into_any_element()
    }
}

/// A hairline that marks where unread messages begin.
fn unread_divider(cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .px_4()
        .py_1()
        .child(div().h(px(1.)).flex_1().bg(cx.theme().danger))
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().danger)
                .child("New"),
        )
        .into_any_element()
}

/// A day separator between two messages that fall on different dates.
pub fn day_divider(label: SharedString, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .child(div().h(px(1.)).flex_1().bg(cx.theme().border))
        .child(
            div()
                .px_2()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().h(px(1.)).flex_1().bg(cx.theme().border))
        .into_any_element()
}

/// Draw one emoji: a character when Unicode knows it, the workspace image when
/// it is a custom upload, and the bare short name when neither applies.
pub fn emoji_glyph(name: &str, index: &EmojiIndex, cx: &App) -> AnyElement {
    match index.lookup(name) {
        Some(Emoji::Unicode(glyph)) => div().child(SharedString::from(glyph)).into_any_element(),
        Some(Emoji::Custom(url)) => img(SharedUri::from(url)).size_4().into_any_element(),
        None => div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(format!(":{name}:")))
            .into_any_element(),
    }
}
