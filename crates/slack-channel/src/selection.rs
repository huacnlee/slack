//! Text that joins the window's selection.
//!
//! A drag over the transcript should pick out characters, the way it does in
//! any other reader, and show what it picked. That takes three things from an
//! element and the first one alone is not enough: it registers its area so the
//! selection knows it exists, it hands over the *laid-out* text so a drag maps
//! to character offsets rather than to the whole block, and it paints the
//! highlight over what came back.
//!
//! Wrapping [`InteractiveText`] rather than replacing it is deliberate. That
//! element is what makes a `<@U…>` mention clickable and gives it a hover
//! card, and selection is not worth losing those for.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, Corners, Edges, Element, ElementId, GlobalElementId, Hitbox,
    InspectorElementId, InteractiveText, IntoElement, LayoutId, PaintQuad, Pixels, Point,
    SharedString, TextLayout, Window, transparent_black,
};
use gpui_base::{TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun};
use gpui_component::ActiveTheme as _;

use slack_api::markup::Block;
use slack_api::models::Ts;

/// How many participants a row may claim before it would tread on the place of
/// the row below it.
const BLOCKS_PER_ROW: u64 = 256;

/// Where each pane's rows begin in the window's reading order.
///
/// Disjoint stretches, so a copy taken in the transcript never interleaves
/// with the thread pane's blocks even though both are on screen at once.
pub const TRANSCRIPT_ORDER: u64 = 0;
pub const THREAD_ORDER: u64 = 1 << 32;

/// Where a pane's `index`-th row starts in the window's reading order.
pub fn reading_order(pane: u64, index: usize) -> u64 {
    pane + index as u64 * BLOCKS_PER_ROW
}

/// The selection participants a transcript hands to its rows.
///
/// One per block rather than one per message, because a block is what a reader
/// sees as a paragraph and expects to select part of. They are owned here, and
/// not made during rendering, because a handle remade every frame would be a
/// selection that vanished the moment anything repainted.
#[derive(Default)]
pub struct Selections(HashMap<Ts, Rc<Vec<TextSelectionHandle>>>);

impl Selections {
    /// Match the participants to what is on screen now.
    ///
    /// Messages keep the handles they already had, so a selection survives a
    /// new message arriving; messages that left the transcript take theirs
    /// with them.
    pub fn refresh<'a>(
        &mut self,
        messages: impl IntoIterator<Item = (&'a Ts, &'a [Block])>,
        cx: &mut App,
    ) {
        let mut next = HashMap::new();
        for (ts, blocks) in messages {
            let mut handles = self
                .0
                .remove(ts)
                .map(|handles| handles.to_vec())
                .unwrap_or_default();
            handles.resize_with(blocks.len().min(BLOCKS_PER_ROW as usize), || {
                TextSelectionHandle::new(String::new(), cx)
            });
            // The fallback is what a copy yields for a block that the
            // selection covers but that was never painted, so it tracks the
            // block's own text.
            for (handle, block) in handles.iter().zip(blocks) {
                handle.set_fallback_copy_text(block.plain_text(), cx);
            }
            next.insert(ts.clone(), Rc::new(handles));
        }
        self.0 = next;
    }

    pub fn get(&self, ts: &Ts) -> Option<Rc<Vec<TextSelectionHandle>>> {
        self.0.get(ts).cloned()
    }
}

/// One run of selectable text.
pub struct SelectableText {
    inner: InteractiveText,
    /// Cloned from the `StyledText` before it was handed over: a `TextLayout`
    /// is a shared handle, so this reads the geometry the element computes.
    layout: TextLayout,
    text: SharedString,
    handle: TextSelectionHandle,
    /// Where this run sits in the window's reading order, which is what makes
    /// a copy come out in the order it was read rather than the order the
    /// renderers happen to be in.
    document_order: u64,
}

impl SelectableText {
    pub fn new(
        inner: InteractiveText,
        layout: TextLayout,
        text: impl Into<SharedString>,
        handle: TextSelectionHandle,
        document_order: u64,
    ) -> Self {
        Self {
            inner,
            layout,
            text: text.into(),
            handle,
            document_order,
        }
    }

    /// The highlight, as one quad per line the selection covers.
    ///
    /// A selection that wraps is three shapes, not one: the tail of the first
    /// line, the whole of the lines between, and the head of the last.
    fn paint_highlight(layout: &TextLayout, range: Range<usize>, window: &mut Window, cx: &App) {
        let (Some(start), Some(end)) = (
            layout.position_for_index(range.start),
            layout.position_for_index(range.end),
        ) else {
            return;
        };

        let bounds = layout.bounds();
        let line_height = layout.line_height();
        let colour = cx.theme().selection;

        for quad in highlight_quads(start, end, bounds, line_height) {
            window.paint_quad(PaintQuad {
                bounds: quad,
                background: colour.into(),
                corner_radii: Corners::default(),
                border_widths: Edges::default(),
                border_color: transparent_black(),
                border_style: BorderStyle::default(),
            });
        }
    }
}

/// The rectangles covering everything between two points in wrapped text.
fn highlight_quads(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }

    let mut quads = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    // The full-width middle only exists when the selection spans three lines
    // or more.
    if end.y > start.y + line_height {
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    quads.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    quads
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        // The inner element keeps its own state under this id, so the wrapper
        // has to claim the same one rather than none.
        self.inner.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (layout_id, ()) = self.inner.request_layout(id, inspector_id, window, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = self
            .inner
            .prepaint(id, inspector_id, bounds, &mut (), window, cx);

        // Registered every frame: the geometry is new each time, while the
        // handle — and so the selection — persists across them.
        self.handle.register(
            TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_document_order(self.document_order)
                .with_text_bounds(vec![bounds]),
            window,
            cx,
        );
        hitbox
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.layout.clone();

        let before = TextSelection::selected_text(window, cx);
        let projection = self.handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)
                    .with_document_order(0),
            ],
            cx,
        );
        // Projecting can change what is selected — a drag that grew into this
        // run, say — and the frame showing it has already been laid out.
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }

        if let Some(range) = projection.ranges().first().cloned().flatten() {
            Self::paint_highlight(&layout, range, window, cx);
        }

        self.inner
            .paint(id, inspector_id, bounds, &mut (), prepaint, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    fn bounds(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(Point::new(px(0.), px(0.)), size(px(width), px(height)))
    }

    #[test]
    fn a_selection_within_one_line_is_a_single_quad() {
        let quads = highlight_quads(
            Point::new(px(10.), px(0.)),
            Point::new(px(90.), px(0.)),
            bounds(200., 20.),
            px(20.),
        );

        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].left(), px(10.));
        assert_eq!(quads[0].right(), px(90.));
    }

    #[test]
    fn a_selection_across_two_lines_is_a_tail_and_a_head() {
        let quads = highlight_quads(
            Point::new(px(120.), px(0.)),
            Point::new(px(40.), px(20.)),
            bounds(200., 40.),
            px(20.),
        );

        assert_eq!(quads.len(), 2, "no full-width line sits between them");
        assert_eq!(
            quads[0].right(),
            px(200.),
            "the first line runs to the edge"
        );
        assert_eq!(quads[1].left(), px(0.), "the last starts at the margin");
    }

    #[test]
    fn a_selection_across_three_lines_fills_the_middle() {
        let quads = highlight_quads(
            Point::new(px(120.), px(0.)),
            Point::new(px(40.), px(40.)),
            bounds(200., 60.),
            px(20.),
        );

        assert_eq!(quads.len(), 3);
        let middle = quads[1];
        assert_eq!(middle.left(), px(0.));
        assert_eq!(middle.right(), px(200.));
        assert_eq!(middle.top(), px(20.));
        assert_eq!(middle.bottom(), px(40.));
    }
}
