//! The Slack app manifest this client asks for.
//!
//! Slack can create an app from a pasted manifest, which is the difference
//! between one paste and adding every scope by hand in a web form. The
//! manifest is generated from [`REQUIRED_SCOPES`] rather than written out
//! twice, so the list the sign-in screen shows and the list Slack is asked for
//! cannot drift apart.

use gpui::SharedString;

/// The app's name in Slack's admin pages and its install dialog.
pub const APP_NAME: &str = "Slack Desktop";

pub const APP_DESCRIPTION: &str = "A native Slack client built with GPUI.";

/// Where an app is created from a manifest.
pub const APP_DOCS: &str = "https://api.slack.com/apps";

/// The user token scopes this client needs.
///
/// Each one buys a feature: the `*:read`/`*:history` pairs the sidebar and the
/// transcript, `chat:write` sending, `reactions:*` reactions, `emoji:read`
/// custom emoji, `users:write` and `dnd:*` presence and notification pausing,
/// the `*:write` family the read marker (so other Slack clients agree with
/// this one about what you have read), `files:write` attachments,
/// `files:read` the image previews in a transcript, `files:write` attaching
/// one, `search:read` message search, and `stars:read` the conversations you
/// starred in Slack.
pub const REQUIRED_SCOPES: &[&str] = &[
    "channels:read",
    "groups:read",
    "im:read",
    "mpim:read",
    // The realtime socket: arrivals and typing, the moment they happen.
    "rtm:stream",
    "channels:history",
    "groups:history",
    "im:history",
    "mpim:history",
    "chat:write",
    "channels:write",
    "groups:write",
    "im:write",
    "mpim:write",
    "users:read",
    "users:write",
    "reactions:read",
    "reactions:write",
    "emoji:read",
    "dnd:read",
    "dnd:write",
    "files:read",
    "files:write",
    "search:read",
    "stars:read",
];

/// The manifest YAML to paste into Slack's "From an app manifest" flow.
///
/// `token_rotation_enabled` is false on purpose: a rotating token would expire
/// out from under the one this client stores in the keychain, and there is no
/// server here to refresh it.
pub fn manifest_yaml() -> SharedString {
    let scopes = REQUIRED_SCOPES
        .iter()
        .map(|scope| format!("      - {scope}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "display_information:\n  \
           name: {APP_NAME}\n  \
           description: {APP_DESCRIPTION}\n  \
           background_color: \"#4a154b\"\n\
         oauth_config:\n  \
           scopes:\n    \
             user:\n\
         {scopes}\n\
         settings:\n  \
           org_deploy_enabled: false\n  \
           socket_mode_enabled: false\n  \
           token_rotation_enabled: false\n"
    )
    .into()
}

/// The scopes as one comma-separated line, which is what Slack's own scope
/// field accepts when adding them to an app that already exists.
///
/// Deliberately has no spaces: this is the value that gets pasted, and a
/// stray space is the kind of thing a form silently keeps.
pub fn scope_list() -> SharedString {
    REQUIRED_SCOPES.join(",").into()
}

/// The same scopes, spaced so a narrow column wraps between them instead of
/// through the middle of `groups:history`.
///
/// This is presentation only — [`scope_list`] is what is copied.
pub fn scope_list_display() -> SharedString {
    REQUIRED_SCOPES.join(", ").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in `manifest.yml` is what the README tells people to use;
    /// this keeps it identical to what the application offers to copy.
    #[test]
    fn the_checked_in_manifest_matches_the_generated_one() {
        let checked_in = include_str!("../../../manifest.yml");
        assert_eq!(
            checked_in,
            manifest_yaml().as_ref(),
            "manifest.yml is stale — regenerate it from crate::manifest::manifest_yaml()"
        );
    }

    #[test]
    fn every_scope_reaches_the_manifest() {
        let yaml = manifest_yaml();
        for scope in REQUIRED_SCOPES {
            assert!(yaml.contains(scope), "{scope} is missing from the manifest");
        }
    }

    #[test]
    fn scopes_are_requested_as_user_scopes_not_bot_scopes() {
        // A bot token cannot read a person's DMs or search their messages, so
        // asking under the wrong key would produce an app that cannot work.
        let yaml = manifest_yaml();
        assert!(yaml.contains("user:"));
        assert!(!yaml.contains("bot:"));
    }

    #[test]
    fn token_rotation_stays_off() {
        assert!(manifest_yaml().contains("token_rotation_enabled: false"));
    }

    #[test]
    fn the_scope_line_is_pasteable() {
        let list = scope_list();
        assert!(!list.contains(' '));
        assert_eq!(list.split(',').count(), REQUIRED_SCOPES.len());
    }

    #[test]
    fn the_displayed_list_carries_the_same_scopes_as_the_copied_one() {
        let copied = scope_list();
        let shown = scope_list_display();
        let copied: Vec<&str> = copied.split(',').map(str::trim).collect();
        let shown: Vec<&str> = shown.split(',').map(str::trim).collect();
        assert_eq!(copied, shown);
    }
}
