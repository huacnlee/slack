//! The loaded slice of one conversation.
//!
//! A transcript is deliberately bounded: it holds a window of recent messages
//! rather than the whole history, so the view can render every row it owns
//! without virtualization and memory does not grow with a long-lived session.
//! Scrolling up extends the window at the top and drops nothing; only new
//! arrivals at the bottom evict the oldest rows.

use std::rc::Rc;

use slack_api::markup::{self, Block};
use slack_api::models::{Message, Ts};

/// The most messages one transcript keeps in memory.
///
/// Slack's own scrollback is effectively unbounded; this is the point past
/// which a desktop pane stops being a reading surface and starts being a
/// memory leak. Reaching it drops the oldest rows, which are already off
/// screen and can be fetched again by scrolling up.
pub const MAX_LOADED: usize = 400;

/// One message with its markup already parsed.
///
/// Parsing happens on arrival rather than in `render`, because a transcript
/// re-renders on every hover and scroll.
#[derive(Debug, Clone)]
pub struct Entry {
    pub message: Message,
    pub blocks: Rc<Vec<Block>>,
    /// Messages and pages quoted inside this one.
    pub quotes: Rc<Vec<crate::quote::Quote>>,
}

impl Entry {
    pub fn new(message: Message) -> Self {
        let blocks = Rc::new(markup::parse(&message.text));
        // Parsed once here rather than per frame: a forwarded message carries
        // its whole body in an attachment, and the transcript re-renders a row
        // every time anything about it changes.
        let quotes = Rc::new(crate::quote::Quote::all(&message.attachments));
        Self {
            message,
            blocks,
            quotes,
        }
    }

    pub fn ts(&self) -> &Ts {
        &self.message.ts
    }
}

/// Messages for one conversation, oldest first.
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn last_ts(&self) -> Option<&Ts> {
        self.entries.last().map(Entry::ts)
    }

    pub fn first_ts(&self) -> Option<&Ts> {
        self.entries.first().map(Entry::ts)
    }

    pub fn get(&self, ts: &Ts) -> Option<&Entry> {
        self.entries.iter().find(|e| e.ts() == ts)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Replace everything with a freshly fetched page.
    pub fn replace(&mut self, messages: Vec<Message>) {
        self.entries = messages.into_iter().map(Entry::new).collect();
        self.sort_and_bound();
    }

    /// Merge messages in, replacing any whose timestamp is already present.
    ///
    /// Slack re-sends an edited message under its original timestamp, so
    /// matching on `ts` is what makes an edit land in place instead of
    /// appearing twice.
    pub fn merge(&mut self, messages: Vec<Message>) {
        for message in messages {
            match self.entries.iter().position(|e| *e.ts() == message.ts) {
                Some(ix) => self.entries[ix] = Entry::new(message),
                None => self.entries.push(Entry::new(message)),
            }
        }
        self.sort_and_bound();
    }

    /// Add an older page fetched by scrolling up.
    pub fn prepend(&mut self, messages: Vec<Message>) {
        let known: Vec<Ts> = self.entries.iter().map(|e| e.ts().clone()).collect();
        let mut older: Vec<Entry> = messages
            .into_iter()
            .filter(|m| !known.contains(&m.ts))
            .map(Entry::new)
            .collect();
        older.append(&mut self.entries);
        self.entries = older;
        self.sort_and_bound();
    }

    pub fn remove(&mut self, ts: &Ts) {
        self.entries.retain(|e| e.ts() != ts);
    }

    /// Replace one message's text in place, keeping its position and its
    /// reactions.
    ///
    /// An edit changes nothing about where a message sits in the transcript,
    /// so re-reading the page around it would only cost a fetch and a scroll
    /// jump to arrive back at the same order.
    pub fn set_text(&mut self, ts: &Ts, text: String, edited: Option<slack_api::models::Edited>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.ts() == ts) {
            entry.message.text = text;
            entry.message.edited = edited;
        }
    }

    /// Update the reaction list of one message in place.
    pub fn set_reactions(&mut self, ts: &Ts, reactions: Vec<slack_api::models::Reaction>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.ts() == ts) {
            entry.message.reactions = reactions;
        }
    }

    fn sort_and_bound(&mut self) {
        self.entries.sort_by(|a, b| a.ts().cmp(b.ts()));
        if self.entries.len() > MAX_LOADED {
            let excess = self.entries.len() - MAX_LOADED;
            self.entries.drain(0..excess);
        }
    }
}

/// One row of a rendered transcript.
///
/// A busy channel is mostly join notices — Slack sends one message per person
/// — and rendering each as its own row buries the conversation. Consecutive
/// ones collapse into a single line instead.
#[derive(Debug)]
pub enum Row<'a> {
    Message(&'a Entry),
    /// A run of consecutive membership notices.
    Joins(Vec<&'a Entry>),
}

/// Group a transcript into rows, collapsing runs of membership notices.
pub fn rows(entries: &[Entry]) -> Vec<Row<'_>> {
    let mut rows = Vec::with_capacity(entries.len());
    let mut run: Vec<&Entry> = Vec::new();

    for entry in entries {
        if is_membership_notice(&entry.message) {
            run.push(entry);
            continue;
        }
        if !run.is_empty() {
            rows.push(Row::Joins(std::mem::take(&mut run)));
        }
        rows.push(Row::Message(entry));
    }
    if !run.is_empty() {
        rows.push(Row::Joins(run));
    }

    rows
}

/// Only joins and leaves collapse. A topic or name change is a real event
/// someone may be looking for, so it keeps its own line.
fn is_membership_notice(message: &Message) -> bool {
    matches!(
        message.subtype.as_deref(),
        Some("channel_join" | "channel_leave" | "group_join" | "group_leave")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(ts: &str, text: &str) -> Message {
        Message {
            ts: Ts(ts.to_string()),
            text: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn merging_orders_by_timestamp_regardless_of_arrival_order() {
        let mut transcript = Transcript::default();
        transcript.merge(vec![message("300.0", "third"), message("100.0", "first")]);
        transcript.merge(vec![message("200.0", "second")]);

        let texts: Vec<&str> = transcript
            .entries()
            .iter()
            .map(|e| e.message.text.as_str())
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn a_repeated_timestamp_replaces_rather_than_duplicates() {
        let mut transcript = Transcript::default();
        transcript.merge(vec![message("100.0", "original")]);
        transcript.merge(vec![message("100.0", "edited")]);

        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript.entries()[0].message.text, "edited");
    }

    #[test]
    fn merging_reparses_the_edited_body() {
        let mut transcript = Transcript::default();
        transcript.merge(vec![message("100.0", "plain")]);
        transcript.merge(vec![message("100.0", "*bold*")]);

        let blocks = &transcript.entries()[0].blocks;
        assert!(matches!(&blocks[0], Block::Paragraph(spans) if spans[0].bold));
    }

    #[test]
    fn prepending_skips_messages_already_loaded() {
        let mut transcript = Transcript::default();
        transcript.merge(vec![message("200.0", "b")]);
        transcript.prepend(vec![message("100.0", "a"), message("200.0", "b again")]);

        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript.entries()[1].message.text, "b");
    }

    #[test]
    fn the_window_drops_the_oldest_rows_once_it_is_full() {
        let mut transcript = Transcript::default();
        let messages: Vec<Message> = (0..MAX_LOADED + 10)
            .map(|i| message(&format!("{}.0", 1000 + i), &i.to_string()))
            .collect();
        transcript.merge(messages);

        assert_eq!(transcript.len(), MAX_LOADED);
        assert_eq!(transcript.entries()[0].message.text, "10");
    }

    fn joined(ts: &str) -> Message {
        Message {
            ts: Ts(ts.to_string()),
            subtype: Some("channel_join".to_string()),
            text: "has joined".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn consecutive_join_notices_collapse_into_one_row() {
        let entries: Vec<Entry> = [joined("100.0"), joined("200.0"), joined("300.0")]
            .into_iter()
            .map(Entry::new)
            .collect();

        let rows = rows(&entries);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], Row::Joins(run) if run.len() == 3));
    }

    #[test]
    fn a_message_between_two_runs_keeps_them_apart() {
        let entries: Vec<Entry> = [
            joined("100.0"),
            message("200.0", "hello"),
            joined("300.0"),
            joined("400.0"),
        ]
        .into_iter()
        .map(Entry::new)
        .collect();

        let rows = rows(&entries);
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], Row::Joins(run) if run.len() == 1));
        assert!(matches!(&rows[1], Row::Message(_)));
        assert!(matches!(&rows[2], Row::Joins(run) if run.len() == 2));
    }

    #[test]
    fn a_topic_change_is_not_collapsed_away() {
        let mut topic = message("100.0", "set the topic");
        topic.subtype = Some("channel_topic".to_string());
        let entries: Vec<Entry> = [topic].into_iter().map(Entry::new).collect();

        assert!(matches!(rows(&entries)[0], Row::Message(_)));
    }

    #[test]
    fn an_ordinary_transcript_is_unchanged() {
        let entries: Vec<Entry> = [message("100.0", "a"), message("200.0", "b")]
            .into_iter()
            .map(Entry::new)
            .collect();

        assert_eq!(rows(&entries).len(), 2);
    }

    #[test]
    fn removing_a_message_leaves_the_rest_in_order() {
        let mut transcript = Transcript::default();
        transcript.merge(vec![
            message("100.0", "a"),
            message("200.0", "b"),
            message("300.0", "c"),
        ]);
        transcript.remove(&Ts("200.0".into()));

        let texts: Vec<&str> = transcript
            .entries()
            .iter()
            .map(|e| e.message.text.as_str())
            .collect();
        assert_eq!(texts, vec!["a", "c"]);
    }
}
