use crate::client::SlackClient;
use crate::error::Result;
use crate::models::DndState;

impl SlackClient {
    pub async fn dnd_info(&self) -> Result<DndState> {
        self.get("dnd.info", &[]).await
    }

    /// Pause notifications for `minutes`.
    pub async fn snooze(&self, minutes: u32) -> Result<DndState> {
        self.post_form("dnd.setSnooze", &[("num_minutes", minutes.to_string())])
            .await
    }

    pub async fn end_snooze(&self) -> Result<DndState> {
        self.post_form("dnd.endSnooze", &[]).await
    }
}
