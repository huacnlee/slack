//! Print the channel the UI test should post into, and the newest message in
//! it, so a keyboard-driven test can verify what actually landed.

use slack_api::{ALL_CONVERSATION_TYPES, SlackClient, store};

fn main() {
    let (token, _) = store::load().unwrap().unwrap();
    let client = SlackClient::new(token).unwrap();

    futures::executor::block_on(async {
        let me = client.auth_test().await.unwrap();
        let conversations = client
            .list_conversations(ALL_CONVERSATION_TYPES)
            .await
            .unwrap();

        let test_channel = conversations.iter().find(|c| c.name == "slack-gpui-test");
        let self_dm = conversations
            .iter()
            .find(|c| c.is_im && c.user.as_deref() == Some(me.user_id.as_str()));

        let (id, label) = match (test_channel, self_dm) {
            (Some(c), _) => (c.id.clone(), format!("#{}", c.name)),
            (None, Some(c)) => (c.id.clone(), me.user.clone()),
            _ => {
                println!("NO_TARGET");
                return;
            }
        };

        println!("TARGET_ID={id}");
        println!("TARGET_LABEL={label}");
        println!("SELF_NAME={}", me.user);

        if let Ok(page) = client.conversation_history(&id, 3, None).await {
            for message in page.messages.iter().rev() {
                println!(
                    "LATEST\t{}\t{}",
                    message.ts,
                    message.text.replace('\n', "\\n")
                );
            }
        }
    });
}
