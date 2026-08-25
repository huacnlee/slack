//! Slack's real-time event stream.
//!
//! Polling can tell you a message exists; it cannot tell you when, and it
//! cannot tell you someone is typing. A user token is allowed to open Slack's
//! RTM socket, which reports both the moment they happen, for every
//! conversation at once — the thing a chat client is actually for.
//!
//! The socket is not a substitute for the Web API. It reports *changes*, so a
//! caller still loads what is there through `conversations.history`; events
//! keep that current afterwards. Nothing is replayed across a disconnect,
//! which is why reconnecting reports [`RtmEvent::Connected`] again: the gap is
//! the caller's to close.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as Frame;

use crate::client::SlackClient;
use crate::error::Result;
use crate::models::{Message, Ts};

/// How long to wait between client pings.
///
/// Slack closes a socket it believes is dead, and a desktop client that has
/// been idle overnight is exactly the case that looks dead.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// How often the client will say it is typing, per conversation.
///
/// Slack repeats its own at about this rate and treats each as good for a few
/// seconds. Sending one per keystroke would say nothing more and put a frame
/// on the wire for every letter.
const TYPING_EVERY: Duration = Duration::from_secs(3);

/// Backoff bounds for reopening a dropped socket.
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// Something that happened in the workspace, as it happened.
#[derive(Debug, Clone)]
pub enum RtmEvent {
    /// The socket is live.
    ///
    /// Also sent after every reconnect. Events that occurred while the socket
    /// was down are *not* replayed, so a caller that wants to be correct
    /// refreshes what it is showing when it sees this.
    Connected,
    /// The socket dropped; a reconnect is in progress.
    Disconnected,
    /// The stream gave up — the token was refused, or RTM is not available to
    /// it. The caller should fall back to polling and stop expecting events.
    Stopped(String),

    Posted {
        channel: String,
        message: Box<Message>,
    },
    Edited {
        channel: String,
        message: Box<Message>,
    },
    Deleted {
        channel: String,
        ts: Ts,
    },
    ReactionChanged {
        channel: String,
        ts: Ts,
        user: String,
        name: String,
        added: bool,
    },
    /// Someone is typing in a conversation. Slack sends no matching "stopped"
    /// event, so this expires on a timer at the other end.
    Typing {
        channel: String,
        user: String,
    },
    PresenceChanged {
        user: String,
        presence: String,
    },
    /// The workspace's shape changed — a channel joined, left, renamed, or a
    /// conversation newly opened. Coarse on purpose: these are rare, and a
    /// refresh is both simpler and more correct than patching each case.
    WorkspaceChanged,
    /// The read marker moved, including from Slack's own clients.
    ReadMarker {
        channel: String,
        ts: Ts,
    },
}

/// What this client says back over the socket.
///
/// Only typing: everything else a client does has a Web API method, and those
/// report failures the socket cannot. Cheap to clone, and safe to call on
/// every keystroke — it decides for itself how often that is worth sending.
#[derive(Clone)]
pub struct RtmSender {
    outbound: mpsc::UnboundedSender<String>,
    last_sent: Arc<Mutex<HashMap<String, Instant>>>,
}

impl RtmSender {
    /// Tell the conversation someone is typing in it.
    ///
    /// Silently does nothing when there is no socket, which is the same thing
    /// it would mean to the people watching.
    pub fn typing(&self, channel: &str) {
        let now = Instant::now();
        {
            let mut last = self
                .last_sent
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(sent) = last.get(channel)
                && now.duration_since(*sent) < TYPING_EVERY
            {
                return;
            }
            last.insert(channel.to_string(), now);
        }

        let frame = json!({ "id": 0, "type": "typing", "channel": channel }).to_string();
        let _ = self.outbound.unbounded_send(frame);
    }
}

impl SlackClient {
    /// Open the real-time stream and keep it open.
    ///
    /// Reconnects on its own for as long as the receiver is held; dropping the
    /// receiver ends the connection. The socket is only ever a supplement, so
    /// a workspace whose token lacks `rtm:stream` gets one
    /// [`RtmEvent::Stopped`] and nothing further rather than an error the
    /// caller has to handle at every use.
    pub fn realtime(&self) -> (RtmSender, mpsc::UnboundedReceiver<RtmEvent>) {
        let (tx, rx) = mpsc::unbounded();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded();
        let client = self.clone();

        self.spawn_on_transport(async move {
            let mut backoff = RECONNECT_MIN;

            loop {
                match client.rtm_connect().await {
                    Ok(url) => match pump(&url, &tx, &mut outbound_rx).await {
                        // A clean close is Slack asking us to reconnect, which
                        // is routine and not worth backing off for.
                        Ok(()) => backoff = RECONNECT_MIN,
                        Err(err) => log::warn!("the realtime socket dropped: {err}"),
                    },
                    Err(err) => {
                        // A refused token will be refused again in a second and
                        // in a minute; saying so once is more useful than
                        // retrying in silence forever.
                        if is_permanent(&err) {
                            let _ = tx.unbounded_send(RtmEvent::Stopped(err.to_string()));
                            return;
                        }
                        log::warn!("could not open the realtime socket: {err}");
                    }
                }

                if tx.unbounded_send(RtmEvent::Disconnected).is_err() {
                    return; // Nobody is listening any more.
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
        });

        (
            RtmSender {
                outbound: outbound_tx,
                last_sent: Arc::new(Mutex::new(HashMap::new())),
            },
            rx,
        )
    }

    /// Ask Slack where to connect, and confirm the token may.
    async fn rtm_connect(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct ConnectReply {
            #[serde(default)]
            url: String,
        }

        let reply: ConnectReply = self
            .get(
                "rtm.connect",
                &[
                    // Presence is polled separately for the few people on
                    // screen; subscribing to everyone would be a firehose.
                    ("presence_sub", "false".to_string()),
                    ("batch_presence_aware", "true".to_string()),
                ],
            )
            .await?;
        Ok(reply.url)
    }
}

/// Whether retrying could ever succeed.
fn is_permanent(err: &crate::Error) -> bool {
    err.is_auth_failure() || err.is_missing_scope()
}

/// Read one connection until it closes, forwarding what it says and sending
/// what it is given.
async fn pump(
    url: &str,
    tx: &mpsc::UnboundedSender<RtmEvent>,
    outbound: &mut mpsc::UnboundedReceiver<String>,
) -> anyhow::Result<()> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
    tx.unbounded_send(RtmEvent::Connected)?;

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await; // The first tick is immediate; skip it.
    let mut ping_id = 0u64;

    loop {
        tokio::select! {
            frame = socket.next() => match frame {
                Some(Ok(Frame::Text(text))) => {
                    if let Some(event) = parse(&text) {
                        // A closed receiver means the window is gone.
                        tx.unbounded_send(event)?;
                    }
                }
                Some(Ok(Frame::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err.into()),
            },
            _ = ping.tick() => {
                ping_id += 1;
                socket
                    .send(Frame::text(json!({ "id": ping_id, "type": "ping" }).to_string()))
                    .await?;
            },
            frame = outbound.next() => match frame {
                Some(frame) => {
                    ping_id += 1;
                    // Slack wants a per-connection id; the caller cannot know
                    // one, so it is stamped here where the connection is.
                    let frame = frame.replacen("\"id\":0", &format!("\"id\":{ping_id}"), 1);
                    socket.send(Frame::text(frame)).await?;
                }
                // The sender was dropped along with everything else.
                None => return Ok(()),
            }
        }
    }
}

/// Turn one frame into an event, or nothing.
///
/// Slack sends far more kinds than a client needs, and the shape of each is
/// only loosely documented. Reading through `Value` lets an unrecognised event
/// be ignored rather than break the stream for everything after it.
fn parse(text: &str) -> Option<RtmEvent> {
    let value: Value = serde_json::from_str(text).ok()?;
    let kind = value.get("type")?.as_str()?;
    let channel = || -> Option<String> { Some(value.get("channel")?.as_str()?.to_string()) };

    match kind {
        "message" => {
            let channel = channel()?;
            match value.get("subtype").and_then(Value::as_str) {
                Some("message_changed") => {
                    let message: Message =
                        serde_json::from_value(value.get("message")?.clone()).ok()?;
                    Some(RtmEvent::Edited {
                        channel,
                        message: Box::new(message),
                    })
                }
                Some("message_deleted") => Some(RtmEvent::Deleted {
                    channel,
                    ts: Ts(value.get("deleted_ts")?.as_str()?.to_string()),
                }),
                // A reply broadcast, a join, a file share: all of them are
                // messages that belong in the transcript.
                _ => {
                    let message: Message = serde_json::from_value(value.clone()).ok()?;
                    Some(RtmEvent::Posted {
                        channel,
                        message: Box::new(message),
                    })
                }
            }
        }

        "reaction_added" | "reaction_removed" => {
            let item = value.get("item")?;
            Some(RtmEvent::ReactionChanged {
                channel: item.get("channel")?.as_str()?.to_string(),
                ts: Ts(item.get("ts")?.as_str()?.to_string()),
                user: value.get("user")?.as_str()?.to_string(),
                name: value.get("reaction")?.as_str()?.to_string(),
                added: kind == "reaction_added",
            })
        }

        "user_typing" => Some(RtmEvent::Typing {
            channel: channel()?,
            user: value.get("user")?.as_str()?.to_string(),
        }),

        "presence_change" => Some(RtmEvent::PresenceChanged {
            user: value.get("user")?.as_str()?.to_string(),
            presence: value.get("presence")?.as_str()?.to_string(),
        }),

        // Slack reports the read marker under a different key per
        // conversation kind, and sends it for every client the reader has
        // open — which is how this window keeps up with their phone.
        "channel_marked" | "group_marked" | "im_marked" | "mpim_marked" => {
            Some(RtmEvent::ReadMarker {
                channel: channel()?,
                ts: Ts(value.get("ts")?.as_str()?.to_string()),
            })
        }

        "channel_joined"
        | "channel_left"
        | "channel_created"
        | "channel_deleted"
        | "channel_rename"
        | "group_joined"
        | "group_left"
        | "group_rename"
        | "im_created"
        | "im_close"
        | "im_open"
        | "mpim_open"
        | "mpim_close"
        | "member_joined_channel"
        | "star_added"
        | "star_removed" => Some(RtmEvent::WorkspaceChanged),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_message_carries_its_channel_and_text() {
        let event =
            parse(r#"{"type":"message","channel":"C1","user":"U1","text":"hi","ts":"1.2"}"#)
                .expect("a message event");

        match event {
            RtmEvent::Posted { channel, message } => {
                assert_eq!(channel, "C1");
                assert_eq!(message.text, "hi");
                assert_eq!(message.ts.as_str(), "1.2");
            }
            other => panic!("expected a post, got {other:?}"),
        }
    }

    #[test]
    fn an_edit_reports_the_message_rather_than_the_event() {
        // The outer `ts` is when the edit happened; the message's own `ts` is
        // what identifies the message being edited.
        let event = parse(
            r#"{"type":"message","subtype":"message_changed","channel":"C1","ts":"9.9",
                "message":{"ts":"1.2","text":"fixed","user":"U1"}}"#,
        )
        .expect("an edit event");

        match event {
            RtmEvent::Edited { message, .. } => {
                assert_eq!(message.ts.as_str(), "1.2");
                assert_eq!(message.text, "fixed");
            }
            other => panic!("expected an edit, got {other:?}"),
        }
    }

    #[test]
    fn a_deletion_names_the_message_it_removed() {
        let event = parse(
            r#"{"type":"message","subtype":"message_deleted","channel":"C1","ts":"9.9","deleted_ts":"1.2"}"#,
        )
        .expect("a delete event");

        assert!(matches!(event, RtmEvent::Deleted { ts, .. } if ts.as_str() == "1.2"));
    }

    #[test]
    fn a_reaction_takes_its_channel_from_the_item_it_is_on() {
        let event = parse(
            r#"{"type":"reaction_added","user":"U1","reaction":"tada",
                "item":{"type":"message","channel":"C1","ts":"1.2"}}"#,
        )
        .expect("a reaction event");

        match event {
            RtmEvent::ReactionChanged {
                channel,
                name,
                added,
                ..
            } => {
                assert_eq!(channel, "C1");
                assert_eq!(name, "tada");
                assert!(added);
            }
            other => panic!("expected a reaction, got {other:?}"),
        }
    }

    #[test]
    fn every_kind_of_marked_event_moves_the_read_marker() {
        for kind in ["channel_marked", "group_marked", "im_marked", "mpim_marked"] {
            let frame = format!(r#"{{"type":"{kind}","channel":"C1","ts":"1.2"}}"#);
            assert!(
                matches!(parse(&frame), Some(RtmEvent::ReadMarker { .. })),
                "{kind} should move the read marker"
            );
        }
    }

    #[test]
    fn an_unknown_event_is_ignored_rather_than_fatal() {
        assert!(parse(r#"{"type":"dnd_updated_user","user":"U1"}"#).is_none());
        assert!(parse("not json at all").is_none());
        assert!(parse(r#"{"no_type":true}"#).is_none());
    }

    #[test]
    fn typing_is_sent_at_most_once_every_few_seconds_per_conversation() {
        let (outbound, mut frames) = mpsc::unbounded();
        let sender = RtmSender {
            outbound,
            last_sent: Arc::new(Mutex::new(HashMap::new())),
        };

        // A composer calls this on every keystroke.
        for _ in 0..20 {
            sender.typing("C1");
        }
        // A different conversation is throttled separately.
        sender.typing("C2");

        let mut sent = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            sent.push(frame);
        }

        assert_eq!(sent.len(), 2, "twenty keystrokes are one thing to say");
        assert!(sent[0].contains("\"channel\":\"C1\""));
        assert!(sent[1].contains("\"channel\":\"C2\""));
    }

    #[test]
    fn a_refused_token_is_not_retried() {
        assert!(is_permanent(&crate::Error::Slack("missing_scope".into())));
        assert!(is_permanent(&crate::Error::Slack("invalid_auth".into())));
        assert!(!is_permanent(&crate::Error::Slack("ratelimited".into())));
    }
}
