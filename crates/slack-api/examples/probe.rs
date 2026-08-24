//! A live diagnostic against the signed-in workspace.
//!
//! Reads the stored token, then exercises every API call the client depends on
//! and reports the shape of what came back. Wire-level breakage — a field that
//! is missing, a reply that does not deserialize, a scope that was never
//! granted — shows up here as a failed line instead of as an empty pane.
//!
//! Run with `cargo run -p slack-api --example probe`.
//!
//! It prints counts and field presence, never the token and never the text of
//! messages outside the channel it creates for itself.

use std::env;

use slack_api::models::Ts;
use slack_api::{ALL_CONVERSATION_TYPES, Result, SlackClient, store};

/// The channel this probe posts into, so it never writes to a real one.
const TEST_CHANNEL: &str = "slack-gpui-test";

#[derive(Default)]
struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn ok(&mut self, name: &str, detail: impl std::fmt::Display) {
        self.passed += 1;
        println!("  PASS  {name:<34} {detail}");
    }

    fn fail(&mut self, name: &str, detail: impl std::fmt::Display) {
        self.failed += 1;
        println!("  FAIL  {name:<34} {detail}");
    }

    fn check<T>(&mut self, name: &str, result: Result<T>, detail: impl Fn(&T) -> String) {
        match result {
            Ok(value) => {
                let text = detail(&value);
                self.ok(name, text)
            }
            Err(err) => self.fail(name, err),
        }
    }
}

fn main() {
    let write = env::args().any(|a| a == "--write");

    let token = match store::load() {
        Ok(Some((token, location))) => {
            println!("token loaded from {location:?}");
            token
        }
        Ok(None) => {
            eprintln!("no token stored — sign in with the client first");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("could not read the token: {err}");
            std::process::exit(1);
        }
    };

    let client = match SlackClient::new(token) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("could not build a client: {err}");
            std::process::exit(1);
        }
    };

    let report = futures::executor::block_on(run(&client, write));

    println!("\n{} passed, {} failed", report.passed, report.failed);
    if report.failed > 0 {
        std::process::exit(1);
    }
}

async fn run(client: &SlackClient, write: bool) -> Report {
    let mut report = Report::default();

    println!("\n— identity —");
    let identity = match client.auth_test().await {
        Ok(identity) => {
            report.ok(
                "auth.test",
                format!("{} as {}", identity.team, identity.user),
            );
            identity
        }
        Err(err) => {
            report.fail("auth.test", err);
            return report;
        }
    };

    println!("\n— workspace —");
    let conversations = client.list_conversations(ALL_CONVERSATION_TYPES).await;
    let conversations = match conversations {
        Ok(list) => {
            let channels = list.iter().filter(|c| !c.kind().is_dm()).count();
            let dms = list.len() - channels;
            report.ok(
                "users.conversations",
                format!("{channels} channels, {dms} DMs"),
            );
            list
        }
        Err(err) => {
            report.fail("users.conversations", err);
            Vec::new()
        }
    };

    report.check("users.list", client.list_users(3000).await, |users| {
        format!("{} members", users.len())
    });
    report.check("emoji.list", client.list_custom_emoji().await, |emoji| {
        format!("{} custom emoji", emoji.len())
    });
    report.check("dnd.info", client.dnd_info().await, |dnd| {
        format!("snoozing={}", dnd.snooze_enabled)
    });
    report.check("users.getPresence", client.presence(None).await, |p| {
        format!("{p:?}")
    });

    println!("\n— reading —");
    let Some(sample) = conversations
        .iter()
        .find(|c| !c.kind().is_dm() && c.is_member)
        .or_else(|| conversations.first())
    else {
        report.fail("conversations.info", "no conversation to read");
        return report;
    };

    report.check(
        "conversations.info",
        client.conversation_info(&sample.id).await,
        |channel| {
            format!(
                "unread={:?} last_read={:?} latest={}",
                channel.unread_count_display,
                channel.last_read.as_ref().map(Ts::as_str),
                channel.latest.is_some()
            )
        },
    );

    let history = client.conversation_history(&sample.id, 20, None).await;
    match history {
        Ok(page) => {
            let with_reactions = page
                .messages
                .iter()
                .filter(|m| !m.reactions.is_empty())
                .count();
            let threads = page
                .messages
                .iter()
                .filter(|m| m.is_thread_parent())
                .count();
            let files = page.messages.iter().filter(|m| !m.files.is_empty()).count();
            let bots = page.messages.iter().filter(|m| m.bot_id.is_some()).count();
            report.ok(
                "conversations.history",
                format!(
                    "{} messages ({with_reactions} reacted, {threads} threaded, {files} with files, {bots} bot)",
                    page.messages.len()
                ),
            );

            if let Some(parent) = page.messages.iter().find(|m| m.is_thread_parent()) {
                report.check(
                    "conversations.replies",
                    client
                        .conversation_replies(&sample.id, &parent.ts, 20)
                        .await,
                    |page| format!("{} in thread", page.messages.len()),
                );
            } else {
                println!("  SKIP  conversations.replies            no thread in the sample");
            }
        }
        Err(err) => report.fail("conversations.history", err),
    }

    report.check(
        "search.messages",
        client.search_messages("the", 5).await,
        |found| format!("{} of {} matches", found.matches.len(), found.total),
    );

    if !write {
        println!("\n(read-only; pass --write to exercise posting)");
        return report;
    }

    println!("\n— writing —");
    // Prefer the dedicated test channel; fall back to a note-to-self so the
    // write path is still exercised on a token that cannot create channels.
    let self_dm = conversations
        .iter()
        .find(|c| c.is_im && c.user.as_deref() == Some(identity.user_id.as_str()))
        .map(|c| c.id.clone());

    let channel = match ensure_test_channel(client, &mut report).await {
        Some(id) => id,
        // Slack always keeps a note-to-self conversation, and it is already in
        // the list, so this needs no extra scope.
        None => match self_dm {
            Some(dm) => {
                report.ok("fallback target", "writing to your own DM instead");
                dm
            }
            None => {
                report.fail("fallback target", "no self-DM in the conversation list");
                return report;
            }
        },
    };

    let posted = client
        .post_message(&channel, "probe: *bold* _italic_ `code` :tada:", None)
        .await;
    let root = match posted {
        Ok(ts) => {
            report.ok("chat.postMessage", format!("ts={ts}"));
            ts
        }
        Err(err) => {
            report.fail("chat.postMessage", err);
            return report;
        }
    };

    report.check(
        "chat.postMessage (thread)",
        client
            .post_message(&channel, "probe: a threaded reply", Some(&root))
            .await,
        |ts| format!("ts={ts}"),
    );
    report.check(
        "reactions.add",
        client.add_reaction(&channel, &root, "eyes").await,
        |_| "added :eyes:".to_string(),
    );
    report.check(
        "chat.update",
        client
            .update_message(&channel, &root, "probe: edited")
            .await,
        |_| "edited".to_string(),
    );
    report.check(
        "chat.getPermalink",
        client.message_permalink(&channel, &root).await,
        |link| format!("{} chars", link.len()),
    );
    report.check(
        "conversations.mark",
        client.mark_read(&channel, &root).await,
        |_| "marked".to_string(),
    );

    // Read it back: this is what proves the write actually landed in the shape
    // the transcript expects, reactions and thread count included.
    match client.conversation_history(&channel, 10, None).await {
        Ok(page) => {
            let mine = page.messages.iter().find(|m| m.ts == root);
            match mine {
                Some(message) => report.ok(
                    "round trip",
                    format!(
                        "text={:?} reactions={} replies={:?} edited={}",
                        message.text,
                        message.reactions.len(),
                        message.reply_count,
                        message.edited.is_some()
                    ),
                ),
                None => report.fail("round trip", "the posted message was not in history"),
            }
        }
        Err(err) => report.fail("round trip", err),
    }

    report.check(
        "files upload",
        client
            .upload_file(
                &channel,
                "probe.txt",
                b"probe attachment".to_vec(),
                Some("probe: an attachment"),
                None,
            )
            .await,
        |file| format!("id={} name={}", file.id, file.display_name()),
    );

    report.check(
        "reactions.remove",
        client.remove_reaction(&channel, &root, "eyes").await,
        |_| "removed".to_string(),
    );

    report
}

/// Find the probe's own channel, creating it the first time.
async fn ensure_test_channel(client: &SlackClient, report: &mut Report) -> Option<String> {
    match client
        .list_conversations("public_channel,private_channel")
        .await
    {
        Ok(list) => {
            if let Some(found) = list.iter().find(|c| c.name == TEST_CHANNEL) {
                report.ok("test channel", format!("#{TEST_CHANNEL} exists"));
                return Some(found.id.clone());
            }
        }
        Err(err) => {
            report.fail("test channel lookup", err);
            return None;
        }
    }

    match client.create_channel(TEST_CHANNEL, false).await {
        Ok(channel) => {
            report.ok("conversations.create", format!("#{TEST_CHANNEL} created"));
            Some(channel.id)
        }
        Err(err) if err.is_missing_scope() => {
            // Creating channels is not something this client does, so the
            // scope is deliberately absent from its manifest.
            println!(
                "  SKIP  conversations.create             needs channels:manage; not a client feature"
            );
            None
        }
        Err(err) => {
            report.fail("conversations.create", err);
            None
        }
    }
}
