//! Signing in with a Slack token.
//!
//! The client authenticates with a user token (`xoxp-…`) issued to a Slack app
//! the reader controls. There is no shared client secret here on purpose: an
//! OAuth code-for-token exchange would have to run on a server that then sees
//! the resulting token, and a desktop client has no business requiring that.
//!
//! The token is checked against `auth.test` before it is stored, so a typo is
//! reported here rather than as a broken workspace one screen later.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    clipboard::Clipboard,
    collapsible::Collapsible,
    h_flex,
    link::Link,
    text::TextView,
    v_flex,
};

use slack_api::models::AuthIdentity;
use slack_api::{SlackClient, store};
use slack_ui::manifest;

#[derive(Clone)]
pub enum SignInEvent {
    /// The token was accepted and stored.
    SignedIn {
        client: SlackClient,
        identity: AuthIdentity,
    },
}

pub struct SignInView {
    token: Entity<InputState>,
    checking: bool,
    error: Option<SharedString>,
    /// Whether the raw scope list is disclosed. Closed by default: the
    /// manifest already carries the scopes, so the list is only needed when
    /// adding them to an app that already exists.
    scopes_open: bool,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SignInEvent> for SignInView {}

impl SignInView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("xoxp-…")
                // A workspace token is a credential; it stays hidden until
                // the reader asks to see what they pasted.
                .masked(true)
        });

        let subscription = cx.subscribe_in(&token, window, |this, _, event, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                // A new attempt clears the previous verdict.
                InputEvent::Change if this.error.take().is_some() => cx.notify(),
                _ => {}
            }
        });

        Self {
            token,
            checking: false,
            error: None,
            scopes_open: false,
            focus: cx.focus_handle(),
            _subscriptions: vec![subscription],
        }
    }

    pub fn focus_token(&self, window: &mut Window, cx: &mut App) {
        let handle = self.token.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        let token = self.token.read(cx).value().trim().to_string();

        let token = match store::validate(&token) {
            Ok(token) => token.to_string(),
            Err(err) => {
                self.error = Some(err.to_string().into());
                cx.notify();
                return;
            }
        };

        self.checking = true;
        self.error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let checked = match SlackClient::new(token.clone()) {
                Ok(client) => match client.auth_test().await {
                    Ok(identity) => Ok((client, identity)),
                    Err(err) => Err(err.to_string()),
                },
                Err(err) => Err(err.to_string()),
            };

            _ = this.update(cx, |this, cx| {
                this.checking = false;
                match checked {
                    Ok((client, identity)) => {
                        // Only a token Slack has accepted is worth keeping.
                        if let Err(err) = store::save(&token) {
                            this.error = Some(
                                format!("Signed in, but the token could not be saved: {err}")
                                    .into(),
                            );
                        }
                        cx.emit(SignInEvent::SignedIn { client, identity });
                    }
                    Err(message) => this.error = Some(message.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl SignInView {
    /// How to get a token, in the order it is actually done.
    ///
    /// Slack can build an app from a pasted manifest, so that is the first
    /// step and the one with the copy button. Adding the scopes one at a time in
    /// a web form is where this setup otherwise goes wrong.
    fn render_setup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_3()
            .pt_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .font_semibold()
                    .text_color(cx.theme().foreground)
                    .child("No token yet?"),
            )
            .child(
                self.render_step(
                    1,
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .flex_wrap()
                                .child("Create an app from a manifest at")
                                .child(
                                    Link::new("slack-apps")
                                        .href(manifest::APP_DOCS)
                                        .child("api.slack.com/apps"),
                                ),
                        )
                        .child(
                            Clipboard::new("copy-manifest")
                                .value(manifest::manifest_yaml())
                                .tooltip("Copy the app manifest")
                                .on_copied(|_, window, cx| {
                                    window.push_notification("Manifest copied", cx)
                                }),
                        ),
                    cx,
                ),
            )
            .child(self.render_step(
                2,
                div().child("Install it to your workspace and approve it."),
                cx,
            ))
            .child(self.render_step(
                3,
                div().child("Copy the User OAuth Token and paste it above."),
                cx,
            ))
            .child(self.render_scopes(cx))
    }

    /// One numbered step: a small ordinal in a fixed column so the step bodies
    /// share one reading edge however long they wrap.
    fn render_step(
        &self,
        number: usize,
        body: impl IntoElement,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .size_4()
                    .rounded_full()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .text_center()
                    .child(SharedString::from(number.to_string())),
            )
            .child(div().flex_1().min_w_0().child(body))
    }

    /// The raw scope list, disclosed on request and copyable as one line.
    fn render_scopes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let scopes = manifest::scope_list();

        Collapsible::new()
            .w_full()
            .gap_2()
            .open(self.scopes_open)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        Button::new("toggle-scopes")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if self.scopes_open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }))
                            .label(SharedString::from(format!(
                                "{} scopes it requests",
                                manifest::REQUIRED_SCOPES.len()
                            )))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.scopes_open = !this.scopes_open;
                                cx.notify();
                            })),
                    )
                    .when(self.scopes_open, |this| {
                        this.child(
                            Clipboard::new("copy-scopes")
                                .value(scopes.clone())
                                .tooltip("Copy the scopes as one line")
                                .on_copied(|_, window, cx| {
                                    window.push_notification("Scopes copied", cx)
                                }),
                        )
                    }),
            )
            .content(
                div()
                    .w_full()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    // Selectable so a single scope can be lifted out, for an
                    // app that already has most of them.
                    .child(
                        TextView::markdown("scopes", manifest::scope_list_display())
                            .selectable(true),
                    ),
            )
    }
}

impl Focusable for SignInView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SignInView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.token.read(cx).value().trim().is_empty();

        // The card scrolls rather than centring rigidly, so disclosing the
        // scope list at the minimum window height cannot clip the token field
        // out of reach.
        div()
            .id("sign-in")
            .size_full()
            .bg(cx.theme().background)
            .track_focus(&self.focus)
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .size_full()
                    .min_h(px(560.))
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(
                        v_flex()
                            .w(px(460.))
                            .gap_6()
                            .p_8()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().secondary)
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div().text_xl().font_semibold().child("Connect to Slack"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                "Paste a user token from a Slack app you control. \
                                         It is stored in your system keychain and sent only \
                                         to slack.com.",
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        Input::new(&self.token)
                                            .mask_toggle()
                                            .cleanable(true)
                                            .prefix(
                                                Icon::new(slack_ui::icons::SlackIcon::Lock).small(),
                                            ),
                                    )
                                    .when_some(self.error.clone(), |this, message| {
                                        this.child(Alert::error("sign-in-error", message))
                                    }),
                            )
                            .child(
                                Button::new("sign-in")
                                    .primary()
                                    .w_full()
                                    .label("Sign in")
                                    .disabled(empty)
                                    .loading(self.checking)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.submit(window, cx)
                                    })),
                            )
                            .child(self.render_setup(cx)),
                    ),
            )
    }
}
