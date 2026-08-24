//! Commands the window can dispatch, and where their keys are bound.
//!
//! Every command has exactly one definition here so a toolbar button, a menu
//! item, and a key binding cannot drift apart.

use gpui::{App, KeyBinding, Menu, MenuItem, actions};

actions!(
    slack,
    [
        /// Jump to a conversation by name, or pick from recent ones.
        OpenQuickSwitcher,
        /// Return to the conversation visited before this one.
        GoBack,
        /// Undo a `GoBack`.
        GoForward,
        /// Search messages across the workspace.
        OpenSearch,
        /// Close the thread pane and return to the transcript.
        CloseThread,
        /// Move keyboard focus to the message composer.
        FocusComposer,
        /// Reload conversations, directory, and emoji.
        Reload,
        /// Switch between the light and dark themes.
        ToggleTheme,
        /// Close the window.
        CloseWindow,
        /// Leave the application.
        Quit,
    ]
);

/// Key context for the application shell.
pub const WORKSPACE_CONTEXT: &str = "SlackWorkspace";

pub fn init(cx: &mut App) {
    // Enter and Shift+Enter inside the composer are deliberately absent: they
    // are the textarea's own `submit_on_enter` contract. Binding them here too
    // would create two owners for one keystroke.
    cx.bind_keys([
        KeyBinding::new("secondary-k", OpenQuickSwitcher, Some(WORKSPACE_CONTEXT)),
        // The platform's own history keys, so they need no explaining.
        KeyBinding::new("secondary-[", GoBack, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("secondary-]", GoForward, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("secondary-f", OpenSearch, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("secondary-r", Reload, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("secondary-shift-t", ToggleTheme, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("escape", CloseThread, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
}

/// The application menu bar.
///
/// Every entry here dispatches the same Action as its in-window control, so a
/// command cannot mean two different things depending on how it was invoked.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Slack").items(vec![
            MenuItem::action("Reload workspace", Reload),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
            MenuItem::action("Quit Slack", Quit),
        ]),
        Menu::new("Go").items(vec![
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
            MenuItem::separator(),
            MenuItem::action("Jump to conversation…", OpenQuickSwitcher),
            MenuItem::action("Search messages…", OpenSearch),
            MenuItem::separator(),
            MenuItem::action("Write a message", FocusComposer),
            MenuItem::action("Close thread", CloseThread),
        ]),
        Menu::new("View").items(vec![MenuItem::action("Switch theme", ToggleTheme)]),
    ]
}
