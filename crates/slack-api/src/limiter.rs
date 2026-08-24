//! Pacing requests so Slack does not have to refuse them.
//!
//! Slack's limits are per method, and it enforces them by answering 429 with a
//! `Retry-After`. Treating that as the only control has two costs: the request
//! that triggered it is delayed by the full window, and every *other* caller of
//! that method is stuck behind it — a background sweep can starve the
//! conversation someone is reading.
//!
//! So the client paces itself. Each method carries a minimum gap between
//! requests, and requests queue against it rather than racing. The gap is
//! learned: Slack's `Retry-After` is the authoritative statement of how long
//! the window is, so being refused once sets the gap for the rest of the
//! session, and a long clean run relaxes it again. That matters because the
//! limits are not knowable up front — `conversations.history` is a request a
//! minute for an app created after May 2025 and far more for an older one, and
//! nothing in the API says which you are.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The gap a method starts with, before Slack has said anything.
///
/// Fast enough that a person waiting on one request does not notice, slow
/// enough that a loop cannot put out hundreds a minute.
const DEFAULT_GAP: Duration = Duration::from_millis(200);

/// Reading messages. This is the one the client calls in a loop — an open
/// conversation, a background unread probe — and the one Slack meters hardest.
const CAREFUL_GAP: Duration = Duration::from_millis(1200);

/// Never pace slower than this, however often we are refused.
const MAX_GAP: Duration = Duration::from_secs(60);

/// After this long without a refusal, try being a little quicker again.
const RELAX_AFTER: Duration = Duration::from_secs(120);

/// What has been learned about one method.
#[derive(Debug)]
struct Pace {
    /// Earliest the next request may be sent.
    next_allowed: Instant,
    /// Minimum gap between requests.
    gap: Duration,
    /// When Slack last refused this method.
    last_refused: Option<Instant>,
}

impl Pace {
    /// Built against the caller's `now`, not a fresh one: a method first seen
    /// on this request has not used its slot yet, and reading the clock again
    /// here would make it look a few nanoseconds late.
    fn new(gap: Duration, now: Instant) -> Self {
        Self {
            next_allowed: now,
            gap,
            last_refused: None,
        }
    }
}

/// Per-method pacing, shared by every clone of a client.
#[derive(Debug, Default)]
pub(crate) struct Limiter {
    methods: Mutex<HashMap<String, Pace>>,
}

impl Limiter {
    /// Claim the next slot for `method`, and say how long to wait for it.
    ///
    /// Reserving before waiting is what makes this a queue: two callers that
    /// arrive together are given consecutive slots rather than the same one.
    pub(crate) fn reserve(&self, method: &str) -> Duration {
        let now = Instant::now();
        let mut methods = self.methods.lock().unwrap_or_else(|e| e.into_inner());
        let pace = methods
            .entry(method.to_string())
            .or_insert_with(|| Pace::new(starting_gap(method), now));

        // A method left alone for a while has probably stopped being busy;
        // let it earn its speed back rather than paying for one bad minute
        // for the rest of the session.
        if let Some(refused) = pace.last_refused
            && now.duration_since(refused) > RELAX_AFTER
        {
            pace.gap = (pace.gap / 2).max(starting_gap(method));
            pace.last_refused = None;
        }

        let wait = pace.next_allowed.saturating_duration_since(now);
        pace.next_allowed = pace.next_allowed.max(now) + pace.gap;
        wait
    }

    /// Slack refused this method and said how long its window is.
    ///
    /// The advertised delay is better information than any guess, so it
    /// becomes the gap outright rather than being averaged into one.
    pub(crate) fn refused(&self, method: &str, retry_after: Duration) {
        let now = Instant::now();
        let mut methods = self.methods.lock().unwrap_or_else(|e| e.into_inner());
        let pace = methods
            .entry(method.to_string())
            .or_insert_with(|| Pace::new(starting_gap(method), now));

        pace.gap = retry_after.max(pace.gap * 2).min(MAX_GAP);
        pace.last_refused = Some(now);
        // Everything already queued behind this method waits out the window
        // too; sending them would only earn another refusal.
        pace.next_allowed = pace.next_allowed.max(now + retry_after);
    }

    /// The current gap for a method, for diagnostics.
    #[cfg(test)]
    fn gap(&self, method: &str) -> Option<Duration> {
        let methods = self.methods.lock().unwrap();
        methods.get(method).map(|p| p.gap)
    }
}

/// How careful to be with a method before Slack has said anything.
fn starting_gap(method: &str) -> Duration {
    match method {
        // Deliberately only the two. The directory and emoji pages are read
        // once at startup and Slack has never objected; pacing them would buy
        // nothing and cost a slower first window.
        "conversations.history" | "conversations.replies" => CAREFUL_GAP,
        _ => DEFAULT_GAP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_request_of_a_method_goes_out_at_once() {
        let limiter = Limiter::default();
        assert_eq!(limiter.reserve("chat.postMessage"), Duration::ZERO);
    }

    #[test]
    fn callers_that_arrive_together_are_given_consecutive_slots() {
        let limiter = Limiter::default();

        let first = limiter.reserve("conversations.history");
        let second = limiter.reserve("conversations.history");
        let third = limiter.reserve("conversations.history");

        assert_eq!(first, Duration::ZERO);
        assert!(second >= CAREFUL_GAP - Duration::from_millis(50));
        assert!(
            third >= second + CAREFUL_GAP - Duration::from_millis(50),
            "the third caller should queue behind the second, not beside it"
        );
    }

    #[test]
    fn methods_are_paced_independently() {
        let limiter = Limiter::default();
        limiter.reserve("conversations.history");

        // Reading history must not delay sending a message.
        assert_eq!(limiter.reserve("chat.postMessage"), Duration::ZERO);
    }

    #[test]
    fn a_refusal_becomes_the_new_gap() {
        let limiter = Limiter::default();
        limiter.reserve("conversations.history");
        limiter.refused("conversations.history", Duration::from_secs(30));

        assert_eq!(
            limiter.gap("conversations.history"),
            Some(Duration::from_secs(30))
        );
        assert!(
            limiter.reserve("conversations.history") >= Duration::from_secs(29),
            "requests queued behind a refusal should wait out the window"
        );
    }

    #[test]
    fn repeated_refusals_do_not_grow_without_bound() {
        let limiter = Limiter::default();
        for _ in 0..10 {
            limiter.refused("conversations.history", Duration::from_secs(30));
        }
        assert_eq!(limiter.gap("conversations.history"), Some(MAX_GAP));
    }

    #[test]
    fn a_method_slack_has_never_refused_keeps_its_starting_gap() {
        let limiter = Limiter::default();
        limiter.reserve("emoji.list");
        assert_eq!(limiter.gap("emoji.list"), Some(DEFAULT_GAP));
    }

    #[test]
    fn only_reading_messages_starts_out_careful() {
        // Startup pages the directory and the conversation list. Those were
        // never what Slack refused, and slowing them shows a slower first
        // window for nothing.
        assert_eq!(starting_gap("conversations.history"), CAREFUL_GAP);
        assert_eq!(starting_gap("conversations.replies"), CAREFUL_GAP);
        assert_eq!(starting_gap("conversations.list"), DEFAULT_GAP);
        assert_eq!(starting_gap("users.list"), DEFAULT_GAP);
    }
}
