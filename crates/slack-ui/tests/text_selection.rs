//! Selecting across messages and copying what was covered.
//!
//! A message row joins the window's text selection by registering its area and
//! the text a copy should produce, rather than its glyphs — see
//! `MessageRow::selection`. This checks the part that is mine to get wrong:
//! that a drag across several rows copies their text, in reading order, and
//! that a drag over one copies only that one.

use gpui::{
    App, AppContext as _, Context, IntoElement, MouseButton, ParentElement as _, Render,
    SharedString, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};
use gpui_base::{ElementExt as _, TextSelectionHandle, TextSelectionRegistration};
use gpui_component::Root;

/// Stands in for a message row: a fixed-height block that registers its area
/// and the text it would copy.
struct Rows {
    rows: Vec<(TextSelectionHandle, SharedString)>,
}

/// Row geometry, chosen so a test can point at one row unambiguously.
const ROW_HEIGHT: f32 = 40.;

impl Rows {
    fn new(texts: &[&str], cx: &mut App) -> Self {
        Self {
            rows: texts
                .iter()
                .map(|text| {
                    (
                        TextSelectionHandle::new(*text, cx),
                        SharedString::from(text.to_string()),
                    )
                })
                .collect(),
        }
    }
}

impl Render for Rows {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .children(self.rows.iter().enumerate().map(|(order, (handle, text))| {
                let handle = handle.clone();
                div()
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .on_prepaint(move |bounds, window, cx| {
                        let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                        handle.register(
                            TextSelectionRegistration::new(hitbox, bounds)
                                .with_document_order(order as u64)
                                .with_text_bounds(vec![bounds]),
                            window,
                            cx,
                        );
                    })
                    .child(text.clone())
            }))
    }
}

fn harness<'a>(texts: &[&str], cx: &'a mut TestAppContext) -> &'a mut VisualTestContext {
    cx.update(gpui_component::init);
    let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
    let (_, cx) = cx.add_window_view(|window, cx| {
        let refs: Vec<&str> = texts.iter().map(|t| t.as_str()).collect();
        let view = cx.new(|cx| Rows::new(&refs, cx));
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    draw(cx);
    cx
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Drag across rows `from`..=`to`.
///
/// The start and end are at different x positions so that a drag within one
/// row is still a drag: a press and release at the same point is a click, and
/// a click selects nothing.
fn drag(cx: &mut VisualTestContext, from: usize, to: usize) {
    let at = |row: usize, x: f32| point(px(x), px(ROW_HEIGHT * row as f32 + ROW_HEIGHT / 2.));

    cx.simulate_mouse_down(at(from, 10.), MouseButton::Left, Default::default());
    cx.simulate_mouse_move(at(to, 90.), Some(MouseButton::Left), Default::default());
    cx.simulate_mouse_up(at(to, 90.), MouseButton::Left, Default::default());
    draw(cx);
}

fn selected(cx: &mut VisualTestContext) -> String {
    cx.update(gpui_base::TextSelection::selected_text)
}

#[gpui::test]
fn a_drag_across_messages_copies_them_in_order(cx: &mut TestAppContext) {
    let cx = harness(&["first message", "second message", "third message"], cx);

    drag(cx, 0, 2);
    let text = selected(cx);

    assert!(text.contains("first message"), "got: {text:?}");
    assert!(text.contains("second message"), "got: {text:?}");
    assert!(text.contains("third message"), "got: {text:?}");
    assert!(
        text.find("first message") < text.find("third message"),
        "reading order was lost: {text:?}"
    );
}

#[gpui::test]
fn a_drag_within_one_message_copies_only_that_one(cx: &mut TestAppContext) {
    let cx = harness(&["first message", "second message", "third message"], cx);

    drag(cx, 1, 1);
    let text = selected(cx);

    assert!(text.contains("second message"), "got: {text:?}");
    assert!(!text.contains("first message"), "got: {text:?}");
    assert!(!text.contains("third message"), "got: {text:?}");
}

#[gpui::test]
fn dragging_upwards_still_reads_downwards(cx: &mut TestAppContext) {
    let cx = harness(&["first message", "second message", "third message"], cx);

    drag(cx, 2, 0);
    let text = selected(cx);

    assert!(
        text.find("first message") < text.find("third message"),
        "a backwards drag should still copy in reading order: {text:?}"
    );
}
