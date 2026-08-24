use std::fmt;

/// Every failure this crate can report.
///
/// `Slack` carries the machine-readable `error` string from a `{"ok": false}`
/// body, so callers can branch on `ratelimited`, `invalid_auth`,
/// `missing_scope`, and friends without string-matching a rendered message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("could not decode the reply from Slack: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{}", describe(.0))]
    Slack(String),
    /// Slack asked us to back off; the value is the advertised delay.
    #[error("rate limited, retry in {0}s")]
    RateLimited(u64),
    #[error("not signed in")]
    NoToken,
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn slack_code(&self) -> Option<&str> {
        match self {
            Error::Slack(code) => Some(code),
            _ => None,
        }
    }

    /// Whether re-authenticating is the only way forward.
    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self.slack_code(),
            Some("invalid_auth" | "not_authed" | "account_inactive" | "token_revoked")
        ) || matches!(self, Error::NoToken)
    }

    pub fn is_missing_scope(&self) -> bool {
        matches!(
            self.slack_code(),
            Some("missing_scope" | "not_allowed_token_type")
        )
    }
}

/// Slack error codes are terse identifiers; show something a person can act on.
fn describe(code: &str) -> impl fmt::Display + '_ {
    let text = match code {
        "invalid_auth" | "not_authed" => "the token was rejected — sign in again",
        "token_revoked" => "the token was revoked — sign in again",
        "account_inactive" => "this account is deactivated",
        "missing_scope" => "the token is missing a permission scope for this request",
        "not_allowed_token_type" => "this request needs a user token",
        "channel_not_found" => "that conversation no longer exists",
        "not_in_channel" => "you are not a member of that channel",
        "is_archived" => "that conversation is archived",
        "msg_too_long" => "that message is too long for Slack",
        "no_permission" => "your account is not allowed to do that",
        "ratelimited" => "Slack is rate limiting this workspace",
        other => return format!("Slack rejected the request ({other})"),
    };
    text.to_string()
}

pub type Result<T> = std::result::Result<T, Error>;
