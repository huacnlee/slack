//! Reading configuration from a `.env` file.
//!
//! The token and a few other choices are environment variables, which is
//! awkward to retype on every launch during development. A `.env` beside the
//! working directory — or in the config directory for an installed copy —
//! sets them once.
//!
//! Existing environment variables always win: a file must never silently
//! override something the caller set deliberately on the command line.
//!
//! This lives beside the token store rather than in the application, because
//! examples and tests read the token too and should find it the same way the
//! application does.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Read one variable from the `.env` files, without touching the environment.
///
/// Used by the token store: a library that quietly rewrote the process
/// environment as a side effect of a lookup would be a surprise.
pub fn get(key: &str) -> Option<String> {
    candidates()
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|contents| parse(&contents))
        .find(|(found, _)| found == key)
        .map(|(_, value)| value)
}

/// Load `.env` from the working directory and the config directory.
///
/// Returns the files that were applied, for logging.
pub fn load() -> Vec<PathBuf> {
    let mut loaded = Vec::new();
    for path in candidates() {
        if apply_file(&path) {
            loaded.push(path);
        }
    }
    loaded
}

fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        paths.push(cwd.join(".env"));
    }
    paths.push(crate::store::config_dir().join(".env"));
    paths
}

fn apply_file(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    for (key, value) in parse(&contents) {
        // SAFETY: this runs before any thread that reads the environment is
        // started, which is the documented requirement for `set_var`.
        if env::var_os(&key).is_none() {
            unsafe { env::set_var(&key, &value) };
        }
    }
    true
}

/// Parse `KEY=VALUE` lines, tolerating the shapes people actually write.
fn parse(contents: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }

        pairs.push((key.to_string(), unquote(value.trim())));
    }

    pairs
}

/// Strip one layer of matching quotes; an unquoted value keeps a trailing
/// comment off the end.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    match value.split_once(" #") {
        Some((before, _)) => before.trim_end().to_string(),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_variable_can_be_read_without_touching_the_environment() {
        // `get` reads the same shapes `parse` does; the file lookup itself is
        // covered by the application actually starting.
        let pairs = parse("SLACK_TOKEN=xoxp-1234567890-abcdef");
        assert_eq!(
            pairs,
            vec![("SLACK_TOKEN".into(), "xoxp-1234567890-abcdef".into())]
        );
    }

    #[test]
    fn plain_assignments_are_read() {
        let pairs = parse("SLACK_DESKTOP_TOKEN_STORE=file\nRUST_LOG=info");
        assert_eq!(
            pairs,
            vec![
                ("SLACK_DESKTOP_TOKEN_STORE".into(), "file".into()),
                ("RUST_LOG".into(), "info".into()),
            ]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let pairs = parse("# a note\n\n  \nKEY=value\n");
        assert_eq!(pairs, vec![("KEY".to_string(), "value".to_string())]);
    }

    #[test]
    fn an_export_prefix_is_tolerated() {
        let pairs = parse("export KEY=value");
        assert_eq!(pairs, vec![("KEY".to_string(), "value".to_string())]);
    }

    #[test]
    fn quotes_are_stripped_but_inner_ones_are_kept() {
        assert_eq!(unquote("\"a b\""), "a b");
        assert_eq!(unquote("'a b'"), "a b");
        assert_eq!(unquote("\"say \"hi\"\""), "say \"hi\"");
    }

    #[test]
    fn a_trailing_comment_is_dropped_from_an_unquoted_value() {
        assert_eq!(unquote("file # the weaker store"), "file");
        // …but a hash inside a value is not a comment.
        assert_eq!(unquote("pass#word"), "pass#word");
    }

    #[test]
    fn a_value_may_contain_equals_signs() {
        let pairs = parse("RUST_LOG=slack_api=debug,slack_ui=info");
        assert_eq!(
            pairs,
            vec![(
                "RUST_LOG".to_string(),
                "slack_api=debug,slack_ui=info".to_string()
            )]
        );
    }

    #[test]
    fn lines_that_are_not_assignments_are_ignored() {
        assert!(parse("just some prose").is_empty());
        assert!(parse("=novalue").is_empty());
        assert!(parse("bad key=value").is_empty());
    }
}
