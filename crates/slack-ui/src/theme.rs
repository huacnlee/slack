//! Theme commands shared by every entry point that offers them.
//!
//! The menu item, the key binding, and any future preference screen all call
//! this, so they cannot disagree about what "switch theme" does.

use gpui::{App, Window};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode};

/// Apply this product's theme decisions on top of the shared defaults.
///
/// Called at startup and after every theme change, because `Theme::change`
/// installs a fresh palette and would otherwise undo them.
pub fn apply_product_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    // A selected conversation is shown by its background alone. The default
    // outline reads as a focus ring, and in a list this dense it draws a box
    // around one row in a column of a thousand.
    theme.list.active_highlight = false;
    Theme::sync_base(cx);
}

/// Start in whichever theme the desktop is already using.
pub fn follow_system_appearance(cx: &mut App) {
    let mode = ThemeMode::from(cx.window_appearance());
    Theme::change(mode, None, cx);
    apply_product_theme(cx);
}

/// Move between the light and dark themes.
pub fn toggle(window: &mut Window, cx: &mut App) {
    let next = if cx.theme().mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(next, Some(window), cx);
    apply_product_theme(cx);
}
