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
/// Only images: everything else renders as a named link, which needs no bytes.
pub fn wanted(files: impl IntoIterator<Item = File>, known: &Thumbnails) -> Vec<File> {
    files
        .into_iter()
        .filter(|file| file.is_image())
        .filter(|file| thumbnail_url(file).is_some())
        .filter(|file| !file.id.is_empty() && known.get(&file.id).is_none())
        .collect()
}

/// The thumbnail to fetch for a file, preferring the smaller one.
pub fn thumbnail_url(file: &File) -> Option<&str> {
    file.thumb_360
        .as_deref()
        .or(file.thumb_720.as_deref())
        .filter(|url| url.starts_with("https://"))
}

/// Fetch one thumbnail into the cache and return where it landed.
///
/// A file already on disk is returned without a request, which is what makes
/// a second launch instant and an offline one work at all.
pub async fn fetch(cache: &Cache, client: &SlackClient, file: &File) -> Option<Arc<Path>> {
    let path = path_for(cache, &file.id);
    if path.exists() {
        return Some(path.into());
    }

    let url = thumbnail_url(file)?;
    let bytes = match client.download(url, MAX_THUMBNAIL_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            log::debug!("could not fetch the thumbnail for {}: {err}", file.id);
            return None;
        }
    };

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
fn path_for(cache: &Cache, file_id: &str) -> PathBuf {
    let name: String = file_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    cache.root().join("thumbnails").join(format!("{name}.img"))
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
    fn only_images_with_a_thumbnail_are_wanted() {
        let mut pdf = image("F2");
        pdf.mimetype = Some("application/pdf".to_string());
        let mut no_thumb = image("F3");
        no_thumb.thumb_360 = None;
        no_thumb.thumb_720 = None;

        let wanted = wanted([image("F1"), pdf, no_thumb], &Thumbnails::default());
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].id, "F1");
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
        let path = path_for(&cache, "../../etc/passwd");
        assert!(path.starts_with(cache.root()));
        assert!(!path.to_string_lossy().contains(".."));
    }
}
