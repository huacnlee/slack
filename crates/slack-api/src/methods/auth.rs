use crate::client::SlackClient;
use crate::error::Result;
use crate::models::AuthIdentity;

impl SlackClient {
    /// Confirm the token and learn who it belongs to.
    ///
    /// This is the first call the application makes; a failure here is the
    /// only reliable signal that the stored token has stopped working.
    pub async fn auth_test(&self) -> Result<AuthIdentity> {
        self.post_form("auth.test", &[]).await
    }
}
