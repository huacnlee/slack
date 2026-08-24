//! Rendering parsed mrkdwn.
//!
//! Each block becomes one text element with styled ranges rather than a row of
//! per-span boxes, so a long paragraph wraps as prose instead of breaking at
//! every emphasis boundary. Mentions and links stay clickable because their
//! ranges are handed to `InteractiveText` alongside the styling.

use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement,
    ParentElement, RenderOnce, SharedString, StrikethroughStyle, Styled, StyledText,
    UnderlineStyle, Window, div, px,
};
use gpui_component::{ActiveTheme, StyledExt as _, v_flex};

use slack_api::emoji::EmojiIndex;
use slack_api::markup::{Block, Link, Span};

/// What a reader clicked inside a message body.
pub type OnLink = Rc<dyn Fn(&Link, &mut Window, &mut App)>;

/// Supplies the text to show for a link whose label Slack omitted.
///
/// Returning `None` keeps whatever the parser produced. Takes the app context
/// rather than a captured copy of the directory: a thousand-member workspace
/// is not something to clone into every message row on every frame.
pub type ResolveName = Rc<dyn Fn(&Link, &App) -> Option<SharedString>>;

/// Builds the card shown when the pointer rests on a link.
pub type HoverLink = Rc<dyn Fn(&Link, &mut Window, &mut App) -> Option<gpui::AnyView>>;

/// A parsed Slack message rendered as text.
#[derive(IntoElement)]
pub struct MessageBody {
    /// Namespaces the per-block element ids; must be stable for the message.
    id: SharedString,
    blocks: Rc<Vec<Block>>,
    emoji: Rc<EmojiIndex>,
    on_link: Option<OnLink>,
    resolve_name: Option<ResolveName>,
    hover_link: Option<HoverLink>,
    /// Renders quieter, for previews inside a sidebar row or search hit.
    muted: bool,
}

impl MessageBody {
    pub fn new(id: impl Into<SharedString>, blocks: Rc<Vec<Block>>, emoji: Rc<EmojiIndex>) -> Self {
        Self {
            id: id.into(),
            blocks,
            emoji,
            on_link: None,
            resolve_name: None,
            hover_link: None,
            muted: false,
        }
    }

    /// Supply the workspace directory so `<@U123>` renders as a person.
    ///
    /// Slack only includes a display name in the escape when the message was
    /// written by a client that bothered to; most carry the bare id, and
    /// without this the transcript is full of `@U0920QES5FH`.
    pub fn resolve_name(mut self, resolve: ResolveName) -> Self {
        self.resolve_name = Some(resolve);
        self
    }

    /// Show a card when the pointer rests on a mention.
    pub fn hover_link(mut self, hover: HoverLink) -> Self {
        self.hover_link = Some(hover);
        self
    }

    pub fn on_link(mut self, handler: OnLink) -> Self {
        self.on_link = Some(handler);
        self
    }

    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }
}

impl RenderOnce for MessageBody {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let text_color = if self.muted {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
        };

        v_flex()
            .gap_1()
            .text_color(text_color)
            .children(self.blocks.iter().enumerate().map(|(ix, block)| {
                render_block(
                    self.id.clone(),
                    ix,
                    block,
                    &self.emoji,
                    self.on_link.clone(),
                    self.resolve_name.clone(),
                    self.hover_link.clone(),
                    cx,
                )
            }))
    }
}

#[allow(clippy::too_many_arguments)]
fn render_block(
    id: SharedString,
    ix: usize,
    block: &Block,
    emoji: &EmojiIndex,
    on_link: Option<OnLink>,
    resolve_name: Option<ResolveName>,
    hover_link: Option<HoverLink>,
    cx: &App,
) -> gpui::AnyElement {
    match block {
        Block::Code(code) => div()
            .w_full()
            .p_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .child(SharedString::from(code.clone()))
            .into_any_element(),

        Block::Quote(spans) => div()
            .w_full()
            .pl_3()
            .border_l_2()
            .border_color(cx.theme().border)
            .text_color(cx.theme().muted_foreground)
            .child(styled_text(
                (id, ix),
                spans,
                emoji,
                on_link,
                resolve_name,
                hover_link,
                cx,
            ))
            .into_any_element(),

        Block::ListItem {
            spans,
            depth,
            ordered,
        } => div()
            .w_full()
            .h_flex()
            .items_start()
            .gap_2()
            // Nesting is indented by the shared scale rather than by literal
            // pixels, so lists keep their rhythm when the window zooms.
            .pl(px(0.).max(px(12.0 * (*depth).min(4) as f32)))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(if *ordered { "1." } else { "•" }),
            )
            .child(div().flex_1().min_w_0().child(styled_text(
                (id, ix),
                spans,
                emoji,
                on_link,
                resolve_name,
                hover_link,
                cx,
            )))
            .into_any_element(),

        Block::Paragraph(spans) if spans.is_empty() => div().into_any_element(),

        Block::Paragraph(spans) => div()
            .w_full()
            .child(styled_text(
                (id, ix),
                spans,
                emoji,
                on_link,
                resolve_name,
                hover_link,
                cx,
            ))
            .into_any_element(),
    }
}

/// Flatten spans into one string plus the ranges that style or link them.
fn styled_text(
    id: impl Into<ElementId>,
    spans: &[Span],
    emoji: &EmojiIndex,
    on_link: Option<OnLink>,
    resolve_name: Option<ResolveName>,
    hover_link: Option<HoverLink>,
    cx: &App,
) -> impl IntoElement {
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut clickable: Vec<Range<usize>> = Vec::new();
    let mut targets: Vec<Link> = Vec::new();

    for span in spans {
        // Slack often sends an id with no label; the workspace knows the name.
        let rendered = match (&span.link, &resolve_name) {
            (Some(link), Some(resolve)) => match resolve(link, cx) {
                Some(text) => text.to_string(),
                None => emoji.render_unicode(&span.text),
            },
            _ => emoji.render_unicode(&span.text),
        };
        let start = text.len();
        text.push_str(&rendered);
        let range = start..text.len();
        if range.is_empty() {
            continue;
        }

        let mut style = HighlightStyle::default();
        if span.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if span.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if span.strike {
            style.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            });
        }
        if span.code {
            style.background_color = Some(cx.theme().muted);
            style.color = Some(cx.theme().foreground);
        }

        if let Some(link) = &span.link {
            style.color = Some(cx.theme().link);
            // Only a real destination gets an underline; a mention is styled
            // by color alone, the way Slack renders it.
            if matches!(link, Link::Url(_)) {
                style.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                });
            }
            clickable.push(range.clone());
            targets.push(link.clone());
        }

        highlights.push((range, style));
    }

    let styled = StyledText::new(SharedString::from(text)).with_highlights(highlights);
    let hover_targets = targets.clone();
    let hover_ranges = clickable.clone();

    InteractiveText::new(id, styled)
        .when_some(on_link, |this, handler| {
            this.on_click(clickable, move |ix, window, cx| {
                if let Some(link) = targets.get(ix) {
                    handler(link, window, cx);
                }
            })
        })
        .when_some(hover_link, |this, hover| {
            // The tooltip builder is given a character offset, so the link
            // under the pointer has to be found by range.
            this.tooltip(move |offset, window, cx| {
                let ix = hover_ranges.iter().position(|r| r.contains(&offset))?;
                hover(hover_targets.get(ix)?, window, cx)
            })
        })
}
