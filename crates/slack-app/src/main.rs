//! A Slack desktop client built on GPUI.

mod assets;

use gpui::{App, AppContext as _, Styled as _, WindowBounds, WindowOptions, px, size};
use gpui_component::{ActiveTheme as _, Root, TitleBar};

use slack_ui::app::SlackApp;

/// A window narrower than this cannot show a sidebar and a transcript at once.
const MIN_WIDTH: f32 = 720.;
const MIN_HEIGHT: f32 = 480.;
const DEFAULT_WIDTH: f32 = 1180.;
const DEFAULT_HEIGHT: f32 = 760.;

fn main() {
    // `--manifest` prints the Slack app manifest and exits, which is how the
    // checked-in `manifest.yml` is regenerated when the scope list changes.
    if std::env::args().any(|arg| arg == "--manifest") {
        print!("{}", slack_ui::manifest::manifest_yaml());
        return;
    }

    // Before anything reads the environment, including the token store.
    let loaded = slack_api::dotenv::load();
    env_logger::init();
    for path in loaded {
        log::info!("loaded settings from {}", path.display());
    }

    let app = gpui_platform::application().with_assets(assets::Assets);

    app.run(move |cx: &mut App| {
        // Must run before anything touches a component.
        gpui_component::init(cx);
        slack_ui::app::init(cx);
        slack_ui::theme::follow_system_appearance(cx);

        // Remote avatars and custom emoji are ordinary images to GPUI; it
        // needs an HTTP client to fetch them.
        match reqwest_client::ReqwestClient::user_agent(concat!(
            "slack-desktop/",
            env!("CARGO_PKG_VERSION")
        )) {
            Ok(client) => cx.set_http_client(std::sync::Arc::new(client)),
            Err(err) => log::warn!("images will not load: {err}"),
        }

        cx.activate(true);

        let bounds = WindowBounds::centered(size(px(DEFAULT_WIDTH), px(DEFAULT_HEIGHT)), cx);

        cx.spawn(async move |cx| {
            // The application draws its own title bar, so the window has to
            // hand it dragging and the traffic-light inset.
            let options = WindowOptions {
                window_bounds: Some(bounds),
                window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
                ..TitleBar::window_options()
            };

            cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| SlackApp::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("could not open the window");
        })
        .detach();
    });
}
