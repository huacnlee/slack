//! The application root.
//!
//! One window shows exactly one of three things: the check that decides
//! whether a stored token still works, the sign-in screen, or the workspace.
//! Keeping that as a single enum means there is no state where two of them are
//! half-rendered at once.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Root, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    h_flex,
    kbd::Kbd,
    v_flex,
};

use slack_api::models::AuthIdentity;
use slack_api::{SlackClient, store};

use crate::auth::sign_in_view::{SignInEvent, SignInView};
use crate::workspace::store::WorkspaceStore;
use crate::workspace::workspace_view::{WorkspaceView, WorkspaceViewEvent};

enum Screen {
    /// Checking a token that was already stored.
    Restoring,
    SignIn(Entity<SignInView>),
    Workspace(Entity<WorkspaceView>),
}

pub struct SlackApp {
    screen: Screen,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SlackApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            screen: Screen::Restoring,
            focus: cx.focus_handle(),
            _subscriptions: Vec::new(),
        };
        this.restore(window, cx);
        this
    }

    /// Try the stored token. A token that no longer works sends the reader to
    /// sign-in rather than into an empty workspace.
    ///
    /// The keychain read happens off the main thread. macOS can put an
    /// authorization dialog in front of it — every rebuilt binary is a new
    /// identity to the keychain — and that dialog is not this application's to
    /// dismiss. Reading inline would freeze the window with nothing on it
    /// until someone noticed the prompt.
    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let stored = cx
                .background_spawn(async {
                    match store::load() {
                        Ok(Some((token, _))) => Some(token),
                        Ok(None) => None,
                        Err(err) => {
                            log::warn!("could not read the stored token: {err}");
                            None
                        }
                    }
                })
                .await;

            let Some(token) = stored else {
                _ = this.update_in(cx, |this, window, cx| this.show_sign_in(window, cx));
                return;
            };

            let checked = match SlackClient::new(token) {
                Ok(client) => client
                    .auth_test()
                    .await
                    .map(|identity| (client, identity))
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };

            _ = this.update_in(cx, |this, window, cx| match checked {
                Ok((client, identity)) => this.show_workspace(client, identity, window, cx),
                Err(message) => {
                    log::info!("stored token rejected: {message}");
                    this.show_sign_in(window, cx);
                }
            });
        })
        .detach();
    }

    fn show_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| SignInView::new(window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let SignInEvent::SignedIn { client, identity } = event;
            this.show_workspace(client.clone(), identity.clone(), window, cx);
        });

        self._subscriptions = vec![subscription];
        self.screen = Screen::SignIn(view.clone());
        window.defer(cx, move |window, cx| {
            view.update(cx, |view, cx| view.focus_token(window, cx));
        });
        cx.notify();
    }

    fn show_workspace(
        &mut self,
        client: SlackClient,
        identity: AuthIdentity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = cx.new(|cx| WorkspaceStore::new(client, identity, cx));
        let view = cx.new(|cx| WorkspaceView::new(store, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let WorkspaceViewEvent::SignedOut = event;
            this.sign_out(window, cx);
        });

        self._subscriptions = vec![subscription];
        self.screen = Screen::Workspace(view);
        cx.notify();
    }

    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = store::clear() {
            log::warn!("could not clear the stored token: {err}");
        }
        self.show_sign_in(window, cx);
    }

    /// The window's own chrome.
    ///
    /// Drawn by the application rather than the system so the quick switcher
    /// can sit in it. That is where a desktop chat client puts navigation, and
    /// it gives the two commands people use most a visible home instead of
    /// living only in a keyboard shortcut nobody was told about.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let signed_in = matches!(self.screen, Screen::Workspace(_));

        TitleBar::new().child(h_flex().w_full().items_center().justify_center().when(
            signed_in,
            |this| {
                this.child(
                    Button::new("jump")
                        .ghost()
                        .small()
                        .w(px(380.))
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .child(
                                    Icon::new(IconName::Search)
                                        .xsmall()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Jump to a conversation"),
                                )
                                .child(Kbd::new(gpui::Keystroke {
                                    modifiers: gpui::Modifiers {
                                        platform: true,
                                        ..Default::default()
                                    },
                                    key: "k".into(),
                                    key_char: None,
                                })),
                        )
                        .on_click(cx.listener(|_, _: &ClickEvent, window, cx| {
                            window.dispatch_action(Box::new(crate::actions::OpenQuickSwitcher), cx);
                        })),
                )
            },
        ))
    }

    fn render_screen(&self, cx: &Context<Self>) -> AnyElement {
        match &self.screen {
            Screen::Restoring => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from("Checking your Slack sign-in…"))
                .into_any_element(),
            Screen::SignIn(view) => view.clone().into_any_element(),
            Screen::Workspace(view) => view.clone().into_any_element(),
        }
    }
}

impl Focusable for SlackApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SlackApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .child(div().flex_1().min_h_0().child(self.render_screen(cx)))
            // Overlay layers belong to the first-level view of the window.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

/// Register this application's key bindings, menu bar, and window commands.
///
/// Call after `gpui_component::init`.
pub fn init(cx: &mut App) {
    crate::actions::init(cx);
    cx.set_menus(crate::actions::menus());
    cx.on_action(|_: &crate::actions::Quit, cx| cx.quit());
}
