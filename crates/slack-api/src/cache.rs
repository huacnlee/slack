//! On-disk cache, so the client opens instantly and keeps working offline.
//!
//! Slack's API is slow to enumerate a large workspace — a thousand
//! conversations and a thousand members is a dozen paged requests — and it
//! offers no bulk unread endpoint to a user token. Both problems have the same
//! answer: remember what was learned last time.
//!
//! Everything here is per team, atomic, and owner-readable only. A cache entry
//! that fails to read is treated as absent rather than as an error: a stale or
//! corrupt cache must never be the reason the application will not start.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Where a cached value came from, so a view can say whether it is showing
/// live data or the last thing it knew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Loaded from the network this session.
    Live,
    /// Loaded from disk and not yet confirmed.
    Cached,
}

/// A namespaced directory of JSON values for one workspace.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The cache for one team.
    ///
    /// Scoping by team id keeps two workspaces from overwriting each other's
    /// conversation list if the token is ever switched.
    pub fn for_team(team_id: &str) -> Self {
        let team = sanitize(team_id);
        Self {
            root: crate::store::cache_dir().join(team),
        }
    }

    /// A cache rooted at an explicit directory, for tests.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read one value. A missing, unreadable, or unparseable entry is `None`.
    pub fn read<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let path = self.path_for(key)?;
        let bytes = fs::read(&path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(value) => Some(value),
            Err(err) => {
                // A schema change makes old entries unreadable; that is
                // expected, and the stale file is cleared rather than retried
                // on every launch.
                log::debug!("discarding unreadable cache entry {key}: {err}");
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Write one value, replacing any previous one atomically.
    pub fn write<T: Serialize>(&self, key: &str, value: &T) {
        if let Err(err) = self.try_write(key, value) {
            // A cache that cannot be written is a performance problem, not a
            // correctness one; the application carries on with live data.
            log::debug!("could not cache {key}: {err}");
        }
    }

    fn try_write<T: Serialize>(&self, key: &str, value: &T) -> io::Result<()> {
        let path = self
            .path_for(key)
            .ok_or_else(|| io::Error::other(format!("invalid cache key {key}")))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            restrict(parent);
        }

        let bytes = serde_json::to_vec(value)?;
        let temp = path.with_extension("tmp");
        fs::write(&temp, &bytes)?;
        restrict(&temp);
        fs::rename(&temp, &path)
    }

    pub fn remove(&self, key: &str) {
        if let Some(path) = self.path_for(key) {
            let _ = fs::remove_file(path);
        }
    }

    /// Forget everything cached for this team.
    pub fn clear(&self) {
        let _ = fs::remove_dir_all(&self.root);
    }

    /// Resolve a key to a path inside the cache root.
    ///
    /// Keys may contain one `/` to group entries (`messages/C123`); everything
    /// else is sanitized, so a channel id from the network can never escape
    /// the cache directory.
    fn path_for(&self, key: &str) -> Option<PathBuf> {
        let mut path = self.root.clone();
        let mut segments = key.split('/').peekable();
        while let Some(segment) = segments.next() {
            let clean = sanitize(segment);
            if clean.is_empty() {
                return None;
            }
            if segments.peek().is_some() {
                path.push(clean);
            } else {
                path.push(format!("{clean}.json"));
            }
        }
        Some(path)
    }
}

/// Reduce a key segment to characters that are safe in a file name.
///
/// Dots are dropped rather than escaped, which is what makes `..` collapse to
/// nothing and be rejected by `path_for`. The `.json` suffix is added by this
/// module, never taken from a key.
fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect()
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    fn temp_cache(label: &str) -> Cache {
        let root = std::env::temp_dir().join(format!("slack-cache-test-{label}"));
        let cache = Cache::at(root);
        cache.clear();
        cache
    }

    #[test]
    fn a_written_value_reads_back() {
        let cache = temp_cache("roundtrip");
        let value = Sample {
            name: "general".into(),
            count: 3,
        };
        cache.write("conversations", &value);
        assert_eq!(cache.read::<Sample>("conversations"), Some(value));
        cache.clear();
    }

    #[test]
    fn a_missing_key_is_none_rather_than_an_error() {
        let cache = temp_cache("missing");
        assert_eq!(cache.read::<Sample>("nothing-here"), None);
    }

    #[test]
    fn a_corrupt_entry_is_discarded_instead_of_failing() {
        let cache = temp_cache("corrupt");
        let path = cache.path_for("broken").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{not json").unwrap();

        assert_eq!(cache.read::<Sample>("broken"), None);
        // …and the bad file is gone, so it is not re-read every launch.
        assert!(!path.exists());
        cache.clear();
    }

    #[test]
    fn grouped_keys_nest_one_directory_deep() {
        let cache = temp_cache("grouped");
        let value = Sample {
            name: "thread".into(),
            count: 1,
        };
        cache.write("messages/C0123456", &value);
        assert_eq!(cache.read::<Sample>("messages/C0123456"), Some(value));
        assert!(cache.root().join("messages").join("C0123456.json").exists());
        cache.clear();
    }

    #[test]
    fn keys_cannot_escape_the_cache_directory() {
        let cache = temp_cache("escape");
        // A traversal segment sanitizes to nothing, and an empty segment is
        // refused rather than silently joined.
        assert_eq!(cache.path_for("../../etc/passwd"), None);
        assert_eq!(cache.path_for("messages/.."), None);

        let path = cache.path_for("messages/C0123456").unwrap();
        assert!(path.starts_with(cache.root()));
    }

    #[test]
    fn a_key_with_awkward_characters_still_resolves() {
        let cache = temp_cache("awkward");
        let path = cache.path_for("messages/C012 3456!").unwrap();
        assert!(path.ends_with("C0123456.json"));
    }

    #[test]
    fn removing_an_entry_leaves_the_rest() {
        let cache = temp_cache("remove");
        let a = Sample {
            name: "a".into(),
            count: 1,
        };
        let b = Sample {
            name: "b".into(),
            count: 2,
        };
        cache.write("a", &a);
        cache.write("b", &b);
        cache.remove("a");

        assert_eq!(cache.read::<Sample>("a"), None);
        assert_eq!(cache.read::<Sample>("b"), Some(b));
        cache.clear();
    }

    #[test]
    fn each_team_gets_its_own_directory() {
        let one = Cache::for_team("T111");
        let two = Cache::for_team("T222");
        assert_ne!(one.root(), two.root());
    }
}
