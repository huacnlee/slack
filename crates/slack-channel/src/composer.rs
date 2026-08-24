//! Writing a message.
//!
//! One composer serves the channel pane, the thread pane, and inline editing.
//! It owns the text and the in-flight state; the owner decides what sending
//! actually means by handling [`ComposerEvent::Submit`].
//!
//! Enter sends and Shift+Enter breaks the line. That split is the textarea's
//! own `submit_on_enter` contract rather than a competing key binding, so the
//! two cannot disagree about which one wins.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use slack_ui::icons::SlackIcon;

/// What the composer asks its owner to do.
#[derive(Debug, Clone)]
pub enum ComposerEvent {
    /// The text should be sent. The composer keeps it until the owner calls
    /// [`Composer::accept`], so a failed send does not lose what was typed.
    Submit(SharedString),
    /// Editing was abandoned.
    Cancel,
    /// The text changed; owners persist it as a draft.
    Changed(SharedString),
    /// The reader asked to attach a file.
    Attach,
}

/// How the composer presents itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerMode {
    /// Writing a new message.
    Compose,
    /// Rewriting an existing one; offers Save and Cancel.
    Edit,
}

pub struct Composer {
    state: Entity<TextareaState>,
    mode: ComposerMode,
    /// Blocks a second submission while the first is still in flight.
    sending: bool,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(
        placeholder: impl Into<SharedString>,
        mode: ComposerMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .auto_grow(1, 8)
                // Enter sends; Shift+Enter still inserts a line break.
                .submit_on_enter(true)
        });

        let subscription =
            cx.subscribe_in(
                &state,
                window,
                |this, state, event, window, cx| match event {
                    InputEvent::Change => {
                        cx.emit(ComposerEvent::Changed(state.read(cx).value()));
                    }
                    InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                    _ => {}
                },
            );

        Self {
            state,
            mode,
            sending: false,
            focus: cx.focus_handle(),
            _subscriptions: vec![subscription],
        }
    }

    pub fn text(&self, cx: &App) -> SharedString {
        self.state.read(cx).value()
    }

    pub fn is_empty(&self, cx: &App) -> bool {
        self.text(cx).trim().is_empty()
    }

    pub fn is_sending(&self) -> bool {
        self.sending
    }

    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        self.state.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx)
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Mark a submission as accepted: clear the text and allow typing again.
    pub fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sending = false;
        self.state.update(cx, |state, cx| state.clean(window, cx));
        cx.notify();
    }

    /// Mark a submission as failed: keep the text so it can be retried.
    pub fn reject(&mut self, cx: &mut Context<Self>) {
        self.sending = false;
        cx.notify();
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let text = self.text(cx);
        if text.trim().is_empty() {
            return;
        }
        self.sending = true;
        cx.emit(ComposerEvent::Submit(text));
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Cancel);
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.is_empty(cx);
        let editing = self.mode == ComposerMode::Edit;

        v_flex()
            .track_focus(&self.focus)
            .w_full()
            .gap_2()
            .p_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                div()
                    .w_full()
                    .max_h(px(200.))
                    .child(Textarea::new(&self.state).appearance(false).bordered(false)),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(h_flex().gap_1().when(!editing, |this| {
                        this.child(
                            Button::new("attach")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(SlackIcon::Paperclip))
                                .tooltip("Attach a file")
                                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                    cx.emit(ComposerEvent::Attach)
                                })),
                        )
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .when(editing, |this| {
                                this.child(
                                    Button::new("cancel-edit")
                                        .ghost()
                                        .small()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.cancel(cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("send")
                                    .primary()
                                    .small()
                                    .icon(Icon::new(SlackIcon::Send))
                                    .label(if editing { "Save" } else { "Send" })
                                    .disabled(empty)
                                    .loading(self.sending)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.submit(window, cx)
                                    })),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    /// Build a composer in a real window, the way the application does.
    fn composer(cx: &mut TestAppContext) -> (Entity<Composer>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
        });
        let (composer, cx) = cx.add_window_view(|window, cx| {
            Composer::new("Write a message", ComposerMode::Compose, window, cx)
        });
        cx.run_until_parked();
        (composer, cx)
    }

    /// Type into the composer's own text state, which is what a keystroke
    /// reaches. Driving the state directly keeps the test about the
    /// composer's contract rather than about key mapping.
    fn type_text(composer: &Entity<Composer>, text: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            composer.update(cx, |composer, cx| composer.set_text(text, window, cx));
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn enter_submits_what_was_typed(cx: &mut TestAppContext) {
        let (composer, cx) = composer(cx);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                if let ComposerEvent::Submit(text) = event {
                    events.borrow_mut().push(text.to_string());
                }
            })
            .detach();
        });

        type_text(&composer, "hello", cx);
        cx.update(|window, cx| {
            composer.update(cx, |composer, cx| composer.submit(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(events.borrow().as_slice(), ["hello"]);
    }

    #[gpui::test]
    fn an_empty_composer_submits_nothing(cx: &mut TestAppContext) {
        let (composer, cx) = composer(cx);
        let count = std::rc::Rc::new(std::cell::Cell::new(0));

        cx.update(|_, cx| {
            let count = count.clone();
            cx.subscribe(&composer, move |_, _: &ComposerEvent, _| {
                count.set(count.get() + 1);
            })
            .detach();
        });

        type_text(&composer, "   \n  ", cx);
        cx.update(|window, cx| {
            composer.update(cx, |composer, cx| composer.submit(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(count.get(), 0, "whitespace is not a message");
    }

    #[gpui::test]
    fn a_second_submit_is_refused_while_the_first_is_in_flight(cx: &mut TestAppContext) {
        let (composer, cx) = composer(cx);
        let count = std::rc::Rc::new(std::cell::Cell::new(0));

        cx.update(|_, cx| {
            let count = count.clone();
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                if matches!(event, ComposerEvent::Submit(_)) {
                    count.set(count.get() + 1);
                }
            })
            .detach();
        });

        type_text(&composer, "once", cx);
        cx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.submit(window, cx);
                // …before the owner has accepted or rejected it.
                composer.submit(window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(count.get(), 1);
    }

    #[gpui::test]
    fn accepting_clears_the_text_and_rejecting_keeps_it(cx: &mut TestAppContext) {
        let (composer, cx) = composer(cx);

        type_text(&composer, "keep me", cx);
        cx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.submit(window, cx);
                composer.reject(cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(composer.read(cx).text(cx), "keep me");
            assert!(!composer.read(cx).is_sending());
        });

        cx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.submit(window, cx);
                composer.accept(window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert!(composer.read(cx).is_empty(cx));
        });
    }
}
