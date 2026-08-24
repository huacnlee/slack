//! Resolving `:shortcode:` to something renderable.
//!
//! Slack sends emoji as short names in message text and reaction names. Most
//! resolve to a Unicode character; the rest are workspace uploads that resolve
//! to an image URL, and those are only known after `emoji.list` has been read.

use std::collections::HashMap;

/// What a short name turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emoji {
    /// A standard Unicode emoji, ready to draw as text.
    Unicode(String),
    /// A workspace custom emoji, to be fetched from this URL.
    Custom(String),
}

/// Short name to image URL for one workspace's custom emoji.
///
/// Slack expresses aliases as `alias:other-name`, so lookups follow the chain
/// rather than returning the literal alias marker.
#[derive(Debug, Clone, Default)]
pub struct EmojiIndex {
    custom: HashMap<String, String>,
}

impl EmojiIndex {
    pub fn new(custom: HashMap<String, String>) -> Self {
        Self { custom }
    }

    pub fn is_empty(&self) -> bool {
        self.custom.is_empty()
    }

    /// Resolve one short name, without colons.
    pub fn lookup(&self, name: &str) -> Option<Emoji> {
        // Slack appends skin-tone modifiers as `::skin-tone-3`.
        let base = name.split("::").next().unwrap_or(name);

        if let Some(unicode) = unicode_for(base) {
            return Some(Emoji::Unicode(unicode.to_string()));
        }

        // Aliases can chain; four hops is more than any real workspace uses
        // and stops a cycle from spinning here.
        let mut current = base;
        for _ in 0..4 {
            let value = self.custom.get(current)?;
            let Some(next) = value.strip_prefix("alias:") else {
                return Some(Emoji::Custom(value.clone()));
            };
            if let Some(unicode) = unicode_for(next) {
                return Some(Emoji::Unicode(unicode.to_string()));
            }
            current = next;
        }
        None
    }

    /// Replace every `:name:` in `text` with its Unicode character, leaving
    /// custom emoji as their short name. Used for previews and window titles.
    pub fn render_unicode(&self, text: &str) -> String {
        if !text.contains(':') {
            return text.to_string();
        }

        let mut out = String::with_capacity(text.len());
        let mut rest = text;

        while let Some(start) = rest.find(':') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match after.find(':') {
                Some(end) if end > 0 && is_shortcode(&after[..end]) => {
                    let name = &after[..end];
                    match self.lookup(name) {
                        Some(Emoji::Unicode(ch)) => out.push_str(&ch),
                        _ => {
                            out.push(':');
                            out.push_str(name);
                            out.push(':');
                        }
                    }
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push(':');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

fn is_shortcode(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | ':'))
}

/// Standard Unicode emoji, via the GitHub short-name set Slack also uses.
pub fn unicode_for(name: &str) -> Option<&'static str> {
    emojis::get_by_shortcode(name).map(|e| e.as_str())
}

/// A small, ordered set for the reaction picker's default view.
pub const FREQUENT_REACTIONS: &[&str] = &[
    "+1",
    "eyes",
    "white_check_mark",
    "tada",
    "heart",
    "joy",
    "thinking_face",
    "rocket",
    "pray",
    "fire",
    "100",
    "raised_hands",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_short_names_resolve_to_unicode() {
        let index = EmojiIndex::default();
        assert_eq!(index.lookup("tada"), Some(Emoji::Unicode("🎉".into())));
        assert_eq!(index.lookup("+1"), Some(Emoji::Unicode("👍".into())));
    }

    #[test]
    fn skin_tone_modifiers_fall_back_to_the_base_emoji() {
        let index = EmojiIndex::default();
        assert_eq!(
            index.lookup("wave::skin-tone-4"),
            Some(Emoji::Unicode("👋".into()))
        );
    }

    #[test]
    fn custom_emoji_resolve_to_their_image() {
        let index = EmojiIndex::new(HashMap::from([(
            "shipit".to_string(),
            "https://emoji.example/shipit.png".to_string(),
        )]));
        assert_eq!(
            index.lookup("shipit"),
            Some(Emoji::Custom("https://emoji.example/shipit.png".into()))
        );
    }

    #[test]
    fn aliases_follow_through_to_their_target() {
        let index = EmojiIndex::new(HashMap::from([
            ("shipit-squirrel".to_string(), "alias:shipit".to_string()),
            (
                "shipit".to_string(),
                "https://emoji.example/shipit.png".to_string(),
            ),
        ]));
        assert_eq!(
            index.lookup("shipit-squirrel"),
            Some(Emoji::Custom("https://emoji.example/shipit.png".into()))
        );
    }

    #[test]
    fn an_unknown_name_resolves_to_nothing() {
        assert_eq!(EmojiIndex::default().lookup("not-an-emoji-at-all"), None);
    }

    #[test]
    fn rendering_replaces_known_names_and_leaves_the_rest() {
        let index = EmojiIndex::default();
        assert_eq!(index.render_unicode("ship it :tada:"), "ship it 🎉");
        assert_eq!(index.render_unicode("10:30 today"), "10:30 today");
        assert_eq!(index.render_unicode("a :nope: b"), "a :nope: b");
    }
}
