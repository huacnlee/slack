//! Fetching the images Slack keeps behind its token.
//!
//! A thumbnail on `files.slack.com` is not public: it needs the same
//! `Authorization` header as the API. GPUI's image loader has no way to send
//! one, so handing it the URL yields a 429 and an error page every time.
//!
//! Instead the client downloads each thumbnail with the authenticated HTTP
//! client, writes it into the workspace cache, and hands GPUI a local path.
//! The consequence is the one the rest of the application already has: images
//! that were fetched once keep rendering offline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slack_api::models::File;
use slack_api::{Cache, SlackClient};

/// The largest thumbnail worth keeping. Slack's `thumb_360` is well inside
/// this; the cap exists so a mislabelled URL cannot fill the cache.
const MAX_THUMBNAIL_BYTES: usize = 4 * 1024 * 1024;

/// A file on a message, with its thumbnail if one has been fetched.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub file: File,
    pub thumbnail: Option<Arc<Path>>,
}

/// Resolved local paths for message attachments, keyed by Slack file id.
#[derive(Debug, Default)]
pub struct Thumbnails {
    paths: HashMap<String, Arc<Path>>,
}

impl Thumbnails {
    /// The local file for `file_id`, if it has been fetched.
    pub fn get(&self, file_id: &str) -> Option<Arc<Path>> {
        self.paths.get(file_id).cloned()
    }

    pub fn insert(&mut self, file_id: String, path: Arc<Path>) {
        self.paths.insert(file_id, path);
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Pair each file with whatever thumbnail is on disk for it.
    pub fn attach(&self, files: &[File]) -> Vec<Attachment> {
        files
            .iter()
            .map(|file| Attachment {
                file: file.clone(),
                thumbnail: self.get(&file.id),
            })
            .collect()
    }
}

/// The files in `messages` that have a thumbnail worth fetching.
///
/// Anything Slack made a thumbnail for — images and videos. Everything else
/// renders as a named link, which needs no bytes.
pub fn wanted(files: impl IntoIterator<Item = File>, known: &Thumbnails) -> Vec<File> {
    files
        .into_iter()
        .filter(|file| thumbnail_url(file).is_some())
        .filter(|file| !file.id.is_empty() && known.get(&file.id).is_none())
        .collect()
}

/// The thumbnail to fetch for a file, preferring the smaller one.
///
/// A video has no `thumb_360`; Slack gives it `thumb_video`, which is a still
/// frame and exactly what a transcript should show for one.
pub fn thumbnail_url(file: &File) -> Option<&str> {
    file.thumb_360
        .as_deref()
        .or(file.thumb_720.as_deref())
        .or(file.thumb_video.as_deref())
        .filter(|url| url.starts_with("https://"))
}

/// Whether these bytes start the way an image does.
///
/// A content-type header would be easier to trust, but Slack's sign-in page
/// comes back as `200 text/html`, so the bytes are the only honest signal.
fn looks_like_an_image(bytes: &[u8]) -> bool {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG: &[u8] = b"\xff\xd8\xff";
    const GIF: &[u8] = b"GIF8";

    bytes.starts_with(PNG)
        || bytes.starts_with(JPEG)
        || bytes.starts_with(GIF)
        // WEBP is `RIFF....WEBP`.
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

/// The file extension to store a thumbnail under.
///
/// GPUI's image loader picks its decoder from the extension, so a cached file
/// saved without one is handed to the SVG parser and fails. Slack's thumbnails
/// are always JPEG or PNG regardless of the original's type — a thumbnail of a
/// GIF or a video is a still.
fn thumbnail_extension(url: &str) -> &'static str {
    let name = url.split('?').next().unwrap_or(url);
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
    {
        Some(ext) if ext == "png" => "png",
        Some(ext) if ext == "gif" => "gif",
        // Slack serves `_360.jpg`, and anything unrecognised is far more
        // likely to be a JPEG than an SVG, which is what the default was.
        _ => "jpg",
    }
}

/// Fetch one thumbnail into the cache and return where it landed.
///
/// A file already on disk is returned without a request, which is what makes
/// a second launch instant and an offline one work at all.
pub async fn fetch(cache: &Cache, client: &SlackClient, file: &File) -> Option<Arc<Path>> {
    let url = thumbnail_url(file)?;
    let path = path_for(cache, &file.id, thumbnail_extension(url));
    if path.exists() {
        return Some(path.into());
    }

    let bytes = match client.download(url, MAX_THUMBNAIL_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            log::debug!("could not fetch the thumbnail for {}: {err}", file.id);
            return None;
        }
    };

    // Slack answers an unauthorized file request with its sign-in page rather
    // than an error status. Writing that to the cache under a `.png` name
    // makes every later render try to decode a web page.
    if !looks_like_an_image(&bytes) {
        log::warn!(
            "Slack did not return an image for {}; the token most likely \
             lacks the files:read scope",
            file.id
        );
        return None;
    }

    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        log::debug!("could not create the thumbnail directory: {err}");
        return None;
    }
    // Written under a temporary name and renamed, so a half-written file is
    // never handed to the image decoder.
    let temp = path.with_extension("part");
    if let Err(err) = std::fs::write(&temp, &bytes) {
        log::debug!("could not write a thumbnail: {err}");
        return None;
    }
    if let Err(err) = std::fs::rename(&temp, &path) {
        log::debug!("could not store a thumbnail: {err}");
        return None;
    }

    Some(path.into())
}

/// Where a file's thumbnail lives. The id is sanitized because it arrives from
/// the network and becomes a file name.
fn path_for(cache: &Cache, file_id: &str, extension: &str) -> PathBuf {
    let name: String = file_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    cache
        .root()
        .join("thumbnails")
        .join(format!("{name}.{extension}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: &str) -> File {
        File {
            id: id.to_string(),
            mimetype: Some("image/png".to_string()),
            thumb_360: Some("https://files.slack.com/x_360.png".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn a_file_with_no_thumbnail_is_not_wanted() {
        let mut no_thumb = image("F3");
        no_thumb.thumb_360 = None;
        no_thumb.thumb_720 = None;
        no_thumb.thumb_video = None;

        let wanted = wanted([image("F1"), no_thumb], &Thumbnails::default());
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].id, "F1");
    }

    #[test]
    fn anything_slack_made_a_thumbnail_for_is_wanted() {
        // Slack renders a first page for a PDF; showing it beats a filename.
        let mut pdf = image("F2");
        pdf.mimetype = Some("application/pdf".to_string());

        assert_eq!(wanted([pdf], &Thumbnails::default()).len(), 1);
    }

    #[test]
    fn a_file_already_fetched_is_not_wanted_again() {
        let mut known = Thumbnails::default();
        known.insert("F1".to_string(), PathBuf::from("/tmp/F1.img").into());

        assert!(wanted([image("F1")], &known).is_empty());
    }

    #[test]
    fn a_plain_http_thumbnail_is_refused() {
        let mut file = image("F1");
        file.thumb_360 = Some("http://files.slack.com/x.png".to_string());
        file.thumb_720 = None;

        assert_eq!(thumbnail_url(&file), None);
    }

    #[test]
    fn the_larger_thumbnail_is_a_fallback() {
        let mut file = image("F1");
        file.thumb_360 = None;
        file.thumb_720 = Some("https://files.slack.com/x_720.png".to_string());

        assert_eq!(
            thumbnail_url(&file),
            Some("https://files.slack.com/x_720.png")
        );
    }

    #[test]
    fn a_file_id_cannot_escape_the_cache_directory() {
        let cache = Cache::at("/tmp/slack-thumbs-test");
        let path = path_for(&cache, "../../etc/passwd", "jpg");
        assert!(path.starts_with(cache.root()));
        assert!(!path.to_string_lossy().contains(".."));
    }

    #[test]
    fn a_web_page_is_not_mistaken_for_an_image() {
        assert!(!looks_like_an_image(b"<!DOCTYPE html><html lang=\"en\">"));
        assert!(!looks_like_an_image(b""));
        assert!(!looks_like_an_image(b"{\"ok\":false}"));
    }

    #[test]
    fn the_formats_slack_serves_are_recognised() {
        assert!(looks_like_an_image(b"\x89PNG\r\n\x1a\nrest"));
        assert!(looks_like_an_image(b"\xff\xd8\xffrest"));
        assert!(looks_like_an_image(b"GIF89a"));
        assert!(looks_like_an_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "));
    }

    #[test]
    fn a_thumbnail_is_stored_under_a_decodable_extension() {
        assert_eq!(thumbnail_extension("https://x/a_360.png"), "png");
        assert_eq!(thumbnail_extension("https://x/a_360.jpg"), "jpg");
        assert_eq!(thumbnail_extension("https://x/a_360.gif"), "gif");
        // No extension, or one nobody recognises, is not an SVG.
        assert_eq!(thumbnail_extension("https://x/a_360"), "jpg");
        assert_eq!(thumbnail_extension("https://x/a.bin?token=1"), "jpg");
    }

    #[test]
    fn a_video_thumbnail_is_wanted_too() {
        let mut video = image("F9");
        video.mimetype = Some("video/mp4".to_string());
        video.thumb_360 = None;
        video.thumb_720 = None;
        video.thumb_video = Some("https://files.slack.com/v_video.jpg".to_string());

        let wanted = wanted([video], &Thumbnails::default());
        assert_eq!(wanted.len(), 1);
    }
}
