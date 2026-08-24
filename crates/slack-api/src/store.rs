//! Where the token lives between runs.
//!
//! The OS keychain is the default, and it is the right default: a Slack user
//! token grants full read and write access to a workspace, and the keychain is
//! the only store on the machine that asks before handing it out.
//!
//! One escape hatch exists because the default has a real cost during
//! development: macOS ties keychain access to a binary's code signature, so a
//! freshly built binary is a new application to the keychain and asks for a
//! password on every launch. `SLACK_TOKEN` in the environment — from a `.env`
//! file, say — is used as-is, and nothing is read from or written to disk.
//!
//! A `0600` file under the config directory remains as a fallback for machines
//! with no usable secret service. It is never preferred over the keychain.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SERVICE: &str = "com.slack-desktop.token";
const ACCOUNT: &str = "default";

/// Supplies the token directly; nothing is read from or written to disk.
const ENV_TOKEN: &str = "SLACK_TOKEN";

/// Which backend answered a load or save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenLocation {
    Keychain,
    File,
    /// Supplied by `SLACK_TOKEN`; not persisted by this application.
    Environment,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not reach the system keychain: {0}")]
    Keychain(String),
    #[error("could not read or write the token file: {0}")]
    File(#[from] io::Error),
    #[error("that does not look like a Slack token (expected xoxp-… or xoxb-…)")]
    Malformed,
}

/// A Slack token as accepted by this application.
///
/// Validating at the boundary means the rest of the code never carries a
/// string that merely might be a token.
pub fn validate(token: &str) -> Result<&str, StoreError> {
    let token = token.trim();
    let has_prefix = token.starts_with("xoxp-") || token.starts_with("xoxb-");
    let body_is_sane = token.len() >= 15
        && token.len() <= 250
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');

    if has_prefix && body_is_sane {
        Ok(token)
    } else {
        Err(StoreError::Malformed)
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("slack-desktop")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("slack-desktop")
}

fn token_file() -> PathBuf {
    config_dir().join("token")
}

/// Read the stored token.
///
/// Returns `Ok(None)` when no token has been stored yet — that is the ordinary
/// first-run state, not an error.
pub fn load() -> Result<Option<(String, TokenLocation)>, StoreError> {
    if let Ok(token) = std::env::var(ENV_TOKEN)
        && !token.trim().is_empty()
    {
        let valid = validate(&token)?;
        return Ok(Some((valid.to_string(), TokenLocation::Environment)));
    }

    if let Some(token) = read_keychain() {
        return Ok(Some((token, TokenLocation::Keychain)));
    }
    Ok(read_token_file()?.map(|token| (token, TokenLocation::File)))
}

fn read_keychain() -> Option<String> {
    let token = keychain_entry()?.get_password().ok()?;
    validate(&token).ok().map(str::to_string)
}

fn read_token_file() -> Result<Option<String>, StoreError> {
    match fs::read_to_string(token_file()) {
        Ok(contents) => match validate(&contents) {
            Ok(valid) => Ok(Some(valid.to_string())),
            // A corrupt file is worth reporting rather than silently ignoring.
            Err(_) => Err(StoreError::Malformed),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(StoreError::File(err)),
    }
}

/// Persist `token` in the selected backend.
pub fn save(token: &str) -> Result<TokenLocation, StoreError> {
    let token = validate(token)?;

    if let Some(entry) = keychain_entry()
        && entry.set_password(token).is_ok()
    {
        // Do not leave a second, weaker copy behind.
        let _ = fs::remove_file(token_file());
        return Ok(TokenLocation::Keychain);
    }

    write_private_file(&token_file(), token)?;
    Ok(TokenLocation::File)
}

/// Forget the token everywhere it may have been written.
pub fn clear() -> Result<(), StoreError> {
    if let Some(entry) = keychain_entry() {
        let _ = entry.delete_credential();
    }
    match fs::remove_file(token_file()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StoreError::File(err)),
    }
}

fn keychain_entry() -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).ok()
}

/// Write `contents` so only the owner can read it, replacing any existing file
/// atomically so a planted symlink is overwritten rather than followed.
fn write_private_file(path: &Path, contents: &str) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, contents)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    }

    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_user_and_bot_tokens() {
        assert!(validate("xoxp-1234567890-abcdef").is_ok());
        assert!(validate("  xoxb-1234567890-abcdef\n").is_ok());
    }

    #[test]
    fn rejects_anything_that_is_not_a_slack_token() {
        assert!(validate("").is_err());
        assert!(validate("hunter2").is_err());
        assert!(validate("xoxp-short").is_err());
        assert!(validate("xoxp-abc def").is_err());
        assert!(validate("xoxa-1234567890-abcdef").is_err());
    }

    #[test]
    fn validation_returns_the_trimmed_token() {
        assert_eq!(
            validate(" xoxp-1234567890-abc ").unwrap(),
            "xoxp-1234567890-abc"
        );
    }
}
