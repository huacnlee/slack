//! The signed-in window: navigation, transcript, and thread side by side.
//!
//! This is the shell. It owns the panes and the commands that move between
//! them, and it forwards failures from any pane to one notification surface so
//! errors are reported in one voice rather than five.

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, px,
};
use gpui_component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_component::{ActiveTheme, WindowExt as _, h_flex, notification::Notification};

use slack_api::models::Ts;

use crate::actions::{
    CloseThread, FocusComposer, OpenQuickSwitcher, OpenSearch, Reload, ToggleTheme,
    WORKSPACE_CONTEXT,
};
use crate::channel::channel_view::{ChannelEvent, ChannelView};
use crate::channel::thread_view::{ThreadEvent, ThreadView};
use crate::search::search_view::{SearchEvent, SearchView};
use crate::workspace::quick_switcher::{QuickSwitcher, QuickSwitcherEvent};
use crate::workspace::sidebar::{SidebarEvent, SidebarView};
use crate::workspace::store::{WorkspaceEvent, WorkspaceStore};

/// Starting width of the navigation pane, and the range it may be dragged to.
///
/// Wide enough for a channel name at the default font, and bounded so it stays
/// visibly subordinate to the transcript however far it is dragged.
const SIDEBAR_DEFAULT_WIDTH: f32 = 260.;
const SIDEBAR_MIN_WIDTH: f32 = 180.;
const SIDEBAR_MAX_WIDTH: f32 = 420.;

/// Starting width of the thread pane, and the range it may be dragged to.
const THREAD_DEFAULT_WIDTH: f32 = 380.;
const THREAD_MIN_WIDTH: f32 = 300.;
const THREAD_MAX_WIDTH: f32 = 640.;

#[derive(Debug, Clone)]
pub enum WorkspaceViewEvent {
    /// The reader signed out, or the token stopped working.
    SignedOut,
}

pub struct WorkspaceView {
    store: Entity<WorkspaceStore>,
    sidebar: Entity<SidebarView>,
    channel: Entity<ChannelView>,
    thread: Option<Entity<ThreadView>>,
    /// Retained so the panes keep their widths across a relayout.
    shell_panes: Entity<ResizableState>,
    work_panes: Entity<ResizableState>,
    quick_switcher: Entity<QuickSwitcher>,
    search: Entity<SearchView>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WorkspaceViewEvent> for WorkspaceView {}

impl WorkspaceView {
    pub fn new(store: Entity<WorkspaceStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(store.clone(), window, cx));
        let channel = cx.new(|cx| ChannelView::new(store.clone(), window, cx));
        let quick_switcher = cx.new(|cx| QuickSwitcher::new(store.clone(), window, cx));
        let search = cx.new(|cx| SearchView::new(store.clone(), window, cx));
        let shell_panes = cx.new(|_| ResizableState::default());
        let work_panes = cx.new(|_| ResizableState::default());

        let subscriptions = vec![
            cx.subscribe_in(&store, window, Self::on_store_event),
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.subscribe_in(&channel, window, Self::on_channel_event),
            cx.subscribe_in(&quick_switcher, window, Self::on_switcher_event),
            cx.subscribe_in(&search, window, Self::on_search_event),
        ];

        Self {
            store,
            sidebar,
            channel,
            thread: None,
            shell_panes,
            work_panes,
            quick_switcher,
            search,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn store(&self) -> &Entity<WorkspaceStore> {
        &self.store
    }

    // ------------------------------------------------------------ commands

    fn open_thread(
        &mut self,
        channel: SharedString,
        root: Ts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Re-opening the same thread should focus it, not rebuild it.
        if let Some(existing) = self.thread.clone() {
            let same = {
                let thread = existing.read(cx);
                thread.channel() == &channel && thread.root() == &root
            };
            if same {
                existing.update(cx, |thread, cx| thread.focus_composer(window, cx));
                return;
            }
        }

        let view = cx.new(|cx| ThreadView::new(self.store.clone(), channel, root, window, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_thread_event);
        self._subscriptions.push(subscription);
        self.thread = Some(view);
        cx.notify();
    }

    fn close_thread(&mut self, _: &CloseThread, _: &mut Window, cx: &mut Context<Self>) {
        if self.thread.take().is_some() {
            cx.notify();
        }
    }

    fn open_quick_switcher(
        &mut self,
        _: &OpenQuickSwitcher,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let switcher = self.quick_switcher.clone();
        switcher.update(cx, |switcher, cx| switcher.reset(window, cx));

        window.open_dialog(cx, move |dialog, _, _| {
            let switcher = switcher.clone();
            dialog
                .close_button(false)
                .p_0()
                .content(move |content, window, cx| {
                    // The palette owns keyboard navigation, so it must have focus
                    // as soon as the dialog appears.
                    let to_focus = switcher.clone();
                    window.defer(cx, move |window, cx| {
                        to_focus.update(cx, |switcher, cx| switcher.focus(window, cx));
                    });
                    content.child(switcher.clone())
                })
        });
    }

    fn open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.search.clone();

        window.open_dialog(cx, move |dialog, _, _| {
            let search = search.clone();
            dialog
                .title("Search messages")
                .content(move |content, window, cx| {
                    let to_focus = search.clone();
                    window.defer(cx, move |window, cx| {
                        to_focus.update(cx, |search, cx| search.focus(window, cx));
                    });
                    content.child(search.clone())
                })
        });
    }

    fn focus_composer(&mut self, _: &FocusComposer, window: &mut Window, cx: &mut Context<Self>) {
        match self.thread.clone() {
            Some(thread) => thread.update(cx, |thread, cx| thread.focus_composer(window, cx)),
            None => {
                let channel = self.channel.clone();
                channel.update(cx, |channel, cx| channel.focus_composer(window, cx))
            }
        }
    }

    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| store.refresh(cx));
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        crate::theme::toggle(window, cx);
    }

    fn select_conversation(
        &mut self,
        id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Moving to another conversation makes the open thread irrelevant.
        self.thread = None;
        self.store.update(cx, |store, cx| store.select(id, cx));
        let channel = self.channel.clone();
        channel.update(cx, |channel, cx| channel.focus_composer(window, cx));
        self.update_window_title(window, cx);
        cx.notify();
    }

    /// Name the window after what it is showing, the way a desktop client
    /// should, and carry the unread total so a hidden window still reports it.
    fn update_window_title(&self, window: &mut Window, cx: &App) {
        let store = self.store.read(cx);
        let team = &store.identity().team;
        let unread = store.total_unread();

        let title = match store.selected() {
            Some(conversation) if conversation.kind.is_dm() => {
                format!("{} · {team}", conversation.name)
            }
            Some(conversation) => format!("#{} · {team}", conversation.name),
            None => team.clone(),
        };
        let title = if unread > 0 {
            format!("({unread}) {title}")
        } else {
            title
        };
        window.set_window_title(&title);
    }

    fn report(&self, message: SharedString, window: &mut Window, cx: &mut App) {
        window.push_notification(Notification::warning(message), cx);
    }

    // ------------------------------------------------------------ events

    fn on_store_event(
        &mut self,
        _: &Entity<WorkspaceStore>,
        event: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspaceEvent::SignedOut => cx.emit(WorkspaceViewEvent::SignedOut),
            WorkspaceEvent::Failed(message) => self.report(message.clone(), window, cx),
            WorkspaceEvent::ConversationsChanged | WorkspaceEvent::SelectionChanged => {
                self.update_window_title(window, cx)
            }
            _ => {}
        }
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<SidebarView>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::Open(id) => self.select_conversation(id.clone(), window, cx),
            SidebarEvent::SignOutRequested => cx.emit(WorkspaceViewEvent::SignedOut),
        }
    }

    fn on_channel_event(
        &mut self,
        _: &Entity<ChannelView>,
        event: &ChannelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ChannelEvent::OpenThread { channel, root } => {
                self.open_thread(channel.clone(), root.clone(), window, cx)
            }
            ChannelEvent::OpenConversation(id) => self.select_conversation(id.clone(), window, cx),
            ChannelEvent::Failed(message) => self.report(message.clone(), window, cx),
        }
    }

    fn on_thread_event(
        &mut self,
        _: &Entity<ThreadView>,
        event: &ThreadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ThreadEvent::Closed => {
                self.thread = None;
                cx.notify();
            }
            // A new reply changes the parent's reply count, which only a
            // fresh page of history reports.
            ThreadEvent::RepliesChanged { .. } => {
                let channel = self.channel.clone();
                channel.update(cx, |channel, cx| channel.refresh(window, cx));
            }
            ThreadEvent::Failed(message) => self.report(message.clone(), window, cx),
        }
    }

    fn on_switcher_event(
        &mut self,
        _: &Entity<QuickSwitcher>,
        event: &QuickSwitcherEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            QuickSwitcherEvent::Confirmed(id) => {
                window.close_dialog(cx);
                self.select_conversation(id.clone(), window, cx);
            }
            QuickSwitcherEvent::Cancelled => window.close_dialog(cx),
        }
    }

    fn on_search_event(
        &mut self,
        _: &Entity<SearchView>,
        event: &SearchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SearchEvent::OpenConversation(id) => {
                window.close_dialog(cx);
                self.select_conversation(id.clone(), window, cx);
            }
            SearchEvent::Dismissed => window.close_dialog(cx),
        }
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let work = match &self.thread {
            Some(thread) => h_resizable("work-panes")
                .with_state(&self.work_panes)
                .child(resizable_panel().child(self.channel.clone()))
                .child(
                    resizable_panel()
                        .size(px(THREAD_DEFAULT_WIDTH))
                        .size_range(px(THREAD_MIN_WIDTH)..px(THREAD_MAX_WIDTH))
                        .child(thread.clone()),
                )
                .into_any_element(),
            None => div()
                .size_full()
                .min_w_0()
                .child(self.channel.clone())
                .into_any_element(),
        };

        h_flex()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close_thread))
            .on_action(cx.listener(Self::open_quick_switcher))
            .on_action(cx.listener(Self::open_search))
            .on_action(cx.listener(Self::focus_composer))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::toggle_theme))
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_resizable("shell-panes")
                    .with_state(&self.shell_panes)
                    .child(
                        resizable_panel()
                            .size(px(SIDEBAR_DEFAULT_WIDTH))
                            .size_range(px(SIDEBAR_MIN_WIDTH)..px(SIDEBAR_MAX_WIDTH))
                            .child(self.sidebar.clone()),
                    )
                    .child(resizable_panel().child(work)),
            )
    }
}
