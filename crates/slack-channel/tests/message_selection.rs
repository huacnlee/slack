//! Selecting text from a real message row.
//!
//! The companion test in `text_selection.rs` uses a stand-in that registers
//! the same way a row does, which proves the selection mechanism but not the
//! row. This drags across actual `MessageRow`s — the thing a reader touches,
//! with its mentions, its hover group and its buttons all present and all able
//! to swallow the mouse before the selection ever sees it.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};
use gpui::{ListAlignment, ListState, list};
use gpui_component::Root;

use slack_api::models::{AuthIdentity, Ts};
use slack_api::{SlackClient, emoji::EmojiIndex, markup};
use slack_channel::message_row::{MessageActions, MessageRow};
use slack_workspace::store::WorkspaceStore;

struct Rows {
    rows: Vec<(Ts, SharedString)>,
    /// One participant per block of each row, the way the transcript hands
    /// them out.
    selections: Vec<Rc<Vec<gpui_base::TextSelectionHandle>>>,
    actions: MessageActions,
    emoji: Rc<EmojiIndex>,
    /// Set when the rows are rendered through a virtualised list, which is
    /// how the transcript renders them.
    list: Option<ListState>,
}

impl Rows {
    fn row(&self, index: usize) -> impl IntoElement {
        let (ts, text) = &self.rows[index];
        MessageRow::new(
            ts.clone(),
            SharedString::from("Ada"),
            Rc::new(markup::parse(text)),
            self.emoji.clone(),
            SharedString::from("U-me"),
            self.actions.clone(),
        )
        .selection(self.selections[index].clone(), index as u64 * 256)
    }
}

impl Render for Rows {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.list.clone() {
            Some(state) => list(
                state,
                cx.processor(|this, index: usize, _, _| this.row(index).into_any_element()),
            )
            .size_full()
            .into_any_element(),
            None => div()
                .size_full()
                .children((0..self.rows.len()).map(|index| self.row(index)))
                .into_any_element(),
        }
    }
}

fn harness<'a>(texts: &[&str], cx: &'a mut TestAppContext) -> &'a mut VisualTestContext {
    build(texts, false, cx)
}

/// The same rows, rendered the way the transcript renders them.
fn virtualised<'a>(texts: &[&str], cx: &'a mut TestAppContext) -> &'a mut VisualTestContext {
    build(texts, true, cx)
}

fn build<'a>(
    texts: &[&str],
    virtualised: bool,
    cx: &'a mut TestAppContext,
) -> &'a mut VisualTestContext {
    cx.update(gpui_component::init);
    let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();

    let (_, cx) = cx.add_window_view(|window, cx| {
        // A client that never reaches the network: this is about layout and
        // the mouse, and no request is made by rendering a row.
        let client = SlackClient::new("xoxp-not-a-real-token").expect("a client");
        let store = cx.new(|cx| WorkspaceStore::new(client, AuthIdentity::default(), cx));

        let rows: Vec<(Ts, SharedString)> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                (
                    Ts(format!("170000000{i}.000100")),
                    SharedString::from(text.clone()),
                )
            })
            .collect();
        let selections = rows
            .iter()
            .map(|(_, text)| {
                Rc::new(
                    markup::parse(text)
                        .iter()
                        .map(|block| gpui_base::TextSelectionHandle::new(block.plain_text(), cx))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        let count = rows.len();
        let view = cx.new(|_| Rows {
            rows,
            selections,
            actions: actions(store),
            emoji: Rc::new(EmojiIndex::default()),
            list: virtualised.then(|| ListState::new(count, ListAlignment::Bottom, px(400.))),
        });
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    draw(cx);
    cx
}

fn actions(store: gpui::Entity<WorkspaceStore>) -> MessageActions {
    MessageActions {
        toggle_reaction: Rc::new(|_, _, _| {}),
        open_thread: Rc::new(|_, _, _| {}),
        start_edit: Rc::new(|_, _, _| {}),
        delete: Rc::new(|_, _, _| {}),
        copy_link: Rc::new(|_, _, _| {}),
        forward: Rc::new(|_, _, _| {}),
        open_file: Rc::new(|_, _, _| {}),
        follow_link: Rc::new(|_, _, _| {}),
        resolve_name: Rc::new(|_, _| None),
        hover_link: Rc::new(|_, _, _| None),
        open_profile: Rc::new(|_, _, _| {}),
        store,
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn selected(cx: &mut VisualTestContext) -> String {
    cx.update(gpui_base::TextSelection::selected_text)
}

#[gpui::test]
fn a_drag_across_real_message_rows_copies_their_text(cx: &mut TestAppContext) {
    let cx = harness(&["first message", "second message"], cx);

    // Begun on the first row's text — a selection anchors in the run under
    // the pointer, so the avatar gutter is not somewhere a drag can start —
    // and carried down past the second.
    cx.simulate_mouse_down(
        point(px(100.), px(40.)),
        MouseButton::Left,
        Default::default(),
    );
    cx.simulate_mouse_move(
        point(px(400.), px(200.)),
        Some(MouseButton::Left),
        Default::default(),
    );
    cx.simulate_mouse_up(
        point(px(400.), px(200.)),
        MouseButton::Left,
        Default::default(),
    );
    draw(cx);

    let copied = selected(cx);
    assert!(
        copied.ends_with("\nsecond message"),
        "a drag should run from where it started into the row below, got {copied:?}"
    );
    assert!(
        !copied.starts_with("first"),
        "and should start at the character it landed on, not the message, got {copied:?}"
    );
}

#[gpui::test]
fn a_drag_still_selects_when_the_rows_are_virtualised(cx: &mut TestAppContext) {
    // The transcript renders through `list`, which handles its own dragging
    // to scroll. If that swallows the mouse, selection works everywhere it is
    // tested and nowhere a reader can reach.
    let cx = virtualised(&["first message", "second message"], cx);

    // Bottom-aligned, so the rows sit at the foot of the viewport and the last
    // one's text is just above it.
    let size = cx.update(|window, _| window.viewport_size());
    let start = point(px(100.), size.height - px(14.));
    cx.simulate_mouse_down(start, MouseButton::Left, Default::default());
    cx.simulate_mouse_move(
        point(size.width - px(8.), size.height - px(4.)),
        Some(MouseButton::Left),
        Default::default(),
    );
    cx.simulate_mouse_up(
        point(size.width - px(8.), size.height - px(4.)),
        MouseButton::Left,
        Default::default(),
    );
    draw(cx);

    let copied = selected(cx);
    assert!(
        copied.ends_with("message"),
        "a drag over a virtualised transcript should select to the end of the row, got {copied:?}"
    );
}

#[gpui::test]
fn a_drag_selects_each_block_of_a_message_separately(cx: &mut TestAppContext) {
    // Two participants for one message, not one: a block is what a reader
    // sees as a paragraph, and copying should keep them apart rather than
    // running them into a single line.
    let cx = harness(&["first message\n&gt; quoted line"], cx);

    let size = cx.update(|window, _| window.viewport_size());
    cx.simulate_mouse_down(
        point(px(100.), px(40.)),
        MouseButton::Left,
        Default::default(),
    );
    cx.simulate_mouse_move(
        point(size.width - px(4.), size.height - px(4.)),
        Some(MouseButton::Left),
        Default::default(),
    );
    cx.simulate_mouse_up(
        point(size.width - px(4.), size.height - px(4.)),
        MouseButton::Left,
        Default::default(),
    );
    draw(cx);

    let copied = selected(cx);
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(lines.len(), 2, "one line per block, got {copied:?}");
    assert!(
        lines[0].ends_with("message"),
        "the paragraph runs to its end, got {copied:?}"
    );
    assert_eq!(lines[1], "quoted line", "the quote copies whole");
}
