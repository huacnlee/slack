//! Sending a message the whole way: real view code, real network, real Slack.
//!
//! The unit tests around the composer prove that Enter submits and that a
//! duplicate is refused; they say nothing about whether a message actually
//! arrives. This drives `ChannelView` in a real window, posts through the same
//! command the composer invokes, and then waits for the message to come back
//! from Slack into the rendered transcript.
//!
//! It needs a signed-in token, so it is `#[ignore]` by default and skips
//! rather than fails when there is none:
//!
//! ```sh
//! cargo test -p slack-ui --test send_end_to_end -- --ignored --nocapture
//! ```
//!
//! It posts to `#slack-gpui-test` when that channel exists, and to your own
//! direct message otherwise — never to a conversation with other people in it.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{AppContext as _, SharedString, TestAppContext, VisualTestContext};

use slack_api::models::AuthIdentity;
use slack_api::{ALL_CONVERSATION_TYPES, SlackClient, store};
use slack_channel::channel_view::ChannelView;
use slack_workspace::store::WorkspaceStore;

/// How long to wait for Slack. Generous: this is a live network call, and a
/// flaky failure here would be worse than a slow pass.
const TIMEOUT: Duration = Duration::from_secs(45);

/// The channel this test prefers, so it never writes to a real one.
const TEST_CHANNEL: &str = "slack-gpui-test";

#[gpui::test]
#[ignore = "talks to the signed-in Slack workspace"]
async fn a_message_sent_through_the_view_comes_back_from_slack(cx: &mut TestAppContext) {
    // This test deliberately touches the network, so the deterministic test
    // scheduler has to be told that real waiting is expected.
    cx.executor().allow_parking();

    let Some((client, identity)) = signed_in() else {
        eprintln!("skipping: no Slack token stored, and none in SLACK_TOKEN");
        return;
    };

    let (target, label) = target_conversation(&client, &identity).await;
    eprintln!("posting to {label} ({target})");

    cx.update(|cx| {
        gpui_component::init(cx);
        // The composer binds its keys through the shared action registry; the
        // shell's menus are not what this test is about.
        slack_ui::actions::init(cx);
    });

    let store =
        cx.update(|cx| cx.new(|cx| WorkspaceStore::new(client.clone(), identity.clone(), cx)));

    let (view, cx) = cx.add_window_view(|window, cx| ChannelView::new(store.clone(), window, cx));

    // The store loads the workspace before anything can be selected.
    wait_for(cx, "the conversation list", |cx| {
        cx.update(|_, cx| !store.read(cx).conversations().is_empty())
    });

    cx.update(|_, cx| {
        store.update(cx, |store, cx| store.select(target.clone(), cx));
    });
    wait_for(cx, "the transcript", |cx| {
        cx.update(|_, cx| {
            view.read(cx).channel().map(|c| c.to_string()) == Some(target.to_string())
        })
    });

    // A marker so the assertion cannot pass on somebody else's message.
    let marker = SharedString::from(format!(
        "end-to-end test {}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    ));

    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.send(marker.clone(), window, cx));
    });

    wait_for(cx, "the message to come back", |cx| {
        cx.update(|_, cx| {
            view.read(cx)
                .message_texts()
                .iter()
                .any(|text| text.contains(marker.as_ref()))
        })
    });

    eprintln!("PASS: {marker:?} is in the rendered transcript");
}

/// The stored token, or `None` when the machine is not signed in.
fn signed_in() -> Option<(SlackClient, AuthIdentity)> {
    let (token, _) = store::load().ok().flatten()?;
    let client = SlackClient::new(token).ok()?;
    let identity = futures::executor::block_on(client.auth_test()).ok()?;
    Some((client, identity))
}

/// Where to post: the dedicated test channel, else the note-to-self.
async fn target_conversation(
    client: &SlackClient,
    identity: &AuthIdentity,
) -> (SharedString, String) {
    let conversations = client
        .list_conversations(ALL_CONVERSATION_TYPES)
        .await
        .expect("the conversation list");

    if let Some(channel) = conversations.iter().find(|c| c.name == TEST_CHANNEL) {
        return (channel.id.clone().into(), format!("#{TEST_CHANNEL}"));
    }

    let self_dm = conversations
        .iter()
        .find(|c| c.is_im && c.user.as_deref() == Some(identity.user_id.as_str()))
        .expect("a note-to-self conversation");

    (
        self_dm.id.clone().into(),
        format!("{} (self DM)", identity.user),
    )
}

/// Drive the window until `ready`, or fail saying what never happened.
///
/// The network runs on the API client's own runtime, not GPUI's, so this
/// alternates draining the window with real waiting rather than only parking.
fn wait_for(
    cx: &mut VisualTestContext,
    what: &str,
    mut ready: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        cx.run_until_parked();
        if ready(cx) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {TIMEOUT:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
