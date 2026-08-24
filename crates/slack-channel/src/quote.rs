//! A message quoted inside another.
//!
//! Slack forwards a message by attaching it: the carrier message's own text is
//! usually empty, and everything the reader is meant to see arrives in
//! `attachments`. A pasted Slack permalink expands the same way, and so does a
//! link to anywhere else — different content, identical shape. Ignoring
//! attachments therefore does not render a plainer message; it renders a blank
//! one.

use std::rc::Rc;

use gpui::{Hsla, SharedString};
use slack_api::markup::{self, Block};
use slack_api::models::{Attachment, File, Ts};

/// What kind of thing was quoted, which decides how its head reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteKind {
    /// Another Slack message — a forward, or a permalink someone pasted.
    Message,
    /// A page from somewhere else that Slack expanded.
    Link,
}

/// One attachment, ready to render.
#[derive(Debug, Clone)]
pub struct Quote {
    pub kind: QuoteKind,
    /// The original author, or the site the link points at.
    pub author: Option<SharedString>,
    /// Their Slack id, when the quote came from this workspace and so can be
    /// hovered and opened like any other name.
    pub author_id: Option<SharedString>,
    pub avatar: Option<SharedString>,
    pub title: Option<SharedString>,
    /// Where the whole quote leads: the original message, or the page.
    pub link: Option<SharedString>,
    pub blocks: Rc<Vec<Block>>,
    pub image: Option<SharedString>,
    /// When the quoted message was posted.
    pub ts: Option<Ts>,
    /// "Posted in #general", as Slack writes it.
    pub footer: Option<SharedString>,
    /// The bar down the left, when the sender chose a colour.
    pub accent: Option<Hsla>,
    pub files: Vec<File>,
}

impl Quote {
    /// Read one attachment, or nothing if it would render empty.
    pub fn from_attachment(attachment: &Attachment) -> Option<Self> {
        let kind = if attachment.is_share || attachment.is_msg_unfurl {
            QuoteKind::Message
        } else {
            QuoteKind::Link
        };

        let text = attachment
            .text
            .as_deref()
            // `fallback` is what Slack writes for a client that cannot render
            // the attachment. That is not us, but it beats showing nothing.
            .or(attachment.fallback.as_deref())
            .unwrap_or_default();

        let author = attachment
            .author_name
            .as_deref()
            .or(attachment.service_name.as_deref())
            .map(|name| SharedString::from(name.to_string()));

        let title = attachment
            .title
            .as_deref()
            .map(|t| SharedString::from(t.to_string()));

        // Something with no words, no heading, no picture and no author is not
        // a quote — it is an empty box with a coloured edge.
        if text.is_empty()
            && title.is_none()
            && author.is_none()
            && attachment.image_url.is_none()
            && attachment.files.is_empty()
        {
            return None;
        }

        Some(Self {
            kind,
            author,
            author_id: attachment
                .author_id
                .as_deref()
                .map(|id| SharedString::from(id.to_string())),
            avatar: image_url(attachment.author_icon.as_deref()),
            title,
            link: attachment
                .from_url
                .as_deref()
                .or(attachment.title_link.as_deref())
                .or(attachment.author_link.as_deref())
                .map(|url| SharedString::from(url.to_string())),
            blocks: Rc::new(markup::parse(text)),
            image: image_url(
                attachment
                    .image_url
                    .as_deref()
                    .or(attachment.thumb_url.as_deref()),
            ),
            ts: attachment.ts.clone(),
            footer: attachment
                .footer
                .as_deref()
                .map(|f| SharedString::from(f.to_string()))
                .or_else(|| {
                    attachment
                        .channel_name
                        .as_deref()
                        .map(|c| SharedString::from(format!("Posted in #{c}")))
                }),
            accent: attachment.color.as_deref().and_then(parse_colour),
            files: attachment.files.clone(),
        })
    }

    /// Read every attachment on a message.
    pub fn all(attachments: &[Attachment]) -> Vec<Quote> {
        attachments
            .iter()
            .filter_map(Quote::from_attachment)
            .collect()
    }
}

/// Keep only images the loader can actually fetch.
///
/// Slack's own file URLs need the token on the request, which the image loader
/// cannot attach, and answer an HTML sign-in page instead. Rendering that as a
/// picture fails noisily and shows nothing either way.
fn image_url(url: Option<&str>) -> Option<SharedString> {
    let url = url?;
    if url.is_empty() || url.contains("files.slack.com") {
        return None;
    }
    // gpui decodes by sniffing the extension; webp is not among what it reads.
    if url.split('?').next().unwrap_or(url).ends_with(".webp") {
        return None;
    }
    Some(SharedString::from(url.to_string()))
}

/// Slack sends either a hex colour or one of three names.
fn parse_colour(colour: &str) -> Option<Hsla> {
    let hex = colour.strip_prefix('#').unwrap_or(colour);
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(gpui::rgb(value).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(text: &str) -> Attachment {
        Attachment {
            is_share: true,
            author_name: Some("Ada".into()),
            text: Some(text.into()),
            channel_name: Some("general".into()),
            ..Attachment::default()
        }
    }

    #[test]
    fn a_forwarded_message_is_read_as_a_quoted_message() {
        let quote = Quote::from_attachment(&share("the original")).expect("a quote");

        assert_eq!(quote.kind, QuoteKind::Message);
        assert_eq!(quote.author.as_deref(), Some("Ada"));
        assert_eq!(quote.footer.as_deref(), Some("Posted in #general"));
        assert!(!quote.blocks.is_empty(), "the text should be parsed");
    }

    #[test]
    fn a_link_preview_is_not_mistaken_for_a_message() {
        let attachment = Attachment {
            service_name: Some("Example".into()),
            title: Some("A page".into()),
            title_link: Some("https://example.com/a".into()),
            ..Attachment::default()
        };

        let quote = Quote::from_attachment(&attachment).expect("a quote");
        assert_eq!(quote.kind, QuoteKind::Link);
        assert_eq!(quote.link.as_deref(), Some("https://example.com/a"));
    }

    #[test]
    fn an_attachment_with_nothing_in_it_is_dropped() {
        assert!(Quote::from_attachment(&Attachment::default()).is_none());
    }

    #[test]
    fn fallback_is_used_when_slack_sends_no_text() {
        let attachment = Attachment {
            fallback: Some("what a plainer client would show".into()),
            ..Attachment::default()
        };
        assert!(Quote::from_attachment(&attachment).is_some());
    }

    #[test]
    fn an_image_the_loader_cannot_fetch_is_not_offered() {
        // Needs the token on the request, which the loader cannot send.
        assert!(image_url(Some("https://files.slack.com/x/y.png")).is_none());
        // gpui has no webp decoder.
        assert!(image_url(Some("https://example.com/a.webp")).is_none());
        assert!(image_url(Some("https://example.com/a.png")).is_some());
    }

    #[test]
    fn a_colour_is_read_with_or_without_its_hash() {
        assert!(parse_colour("#2eb886").is_some());
        assert!(parse_colour("2eb886").is_some());
        assert!(parse_colour("good").is_none());
    }
}
