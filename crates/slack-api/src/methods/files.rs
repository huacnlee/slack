use serde::Deserialize;
use serde_json::json;

use crate::client::SlackClient;
use crate::error::{Error, Result};
use crate::models::{File, Ts};

#[derive(Deserialize)]
struct UploadUrlReply {
    #[serde(default)]
    upload_url: String,
    #[serde(default)]
    file_id: String,
}

#[derive(Deserialize)]
struct CompleteReply {
    #[serde(default)]
    files: Vec<File>,
}

/// Slack's external upload flow caps a single file well below this; the guard
/// exists so a mistaken selection fails locally instead of after a long POST.
pub const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

impl SlackClient {
    /// Upload `bytes` as `filename` and share it into `channel`.
    ///
    /// This is Slack's three-step external upload: reserve a URL, PUT the
    /// bytes to it, then tell Slack where to share the finished file. The
    /// older single-shot `files.upload` was retired in 2025.
    pub async fn upload_file(
        &self,
        channel: &str,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_ts: Option<&Ts>,
    ) -> Result<File> {
        if bytes.is_empty() {
            return Err(Error::Other("that file is empty".into()));
        }
        if bytes.len() as u64 > MAX_UPLOAD_BYTES {
            return Err(Error::Other("that file is too large to upload".into()));
        }

        let reserved: UploadUrlReply = self
            .get(
                "files.getUploadURLExternal",
                &[
                    ("filename", filename.to_string()),
                    ("length", bytes.len().to_string()),
                ],
            )
            .await?;

        if reserved.upload_url.is_empty() || reserved.file_id.is_empty() {
            return Err(Error::Other("Slack did not return an upload URL".into()));
        }

        self.put_bytes(&reserved.upload_url, bytes).await?;

        let mut body = json!({
            "files": [{ "id": reserved.file_id, "title": filename }],
            "channel_id": channel,
        });
        if let Some(text) = comment.filter(|c| !c.trim().is_empty()) {
            body["initial_comment"] = json!(text);
        }
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts.as_str());
        }

        let reply: CompleteReply = self.post_json("files.completeUploadExternal", body).await?;
        reply
            .files
            .into_iter()
            .next()
            .ok_or_else(|| Error::Other("Slack accepted the upload but returned no file".into()))
    }
}
