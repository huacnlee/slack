use serde_json::Value;
use slack_api::{SlackClient, store};

fn main() {
    let (token, _) = store::load().unwrap().unwrap();
    let client = SlackClient::new(token).unwrap();
    futures::executor::block_on(async {
        let me = client.auth_test().await.unwrap();
        for q in [
            format!("@{}", me.user),
            format!("to:@{}", me.user),
            format!("<@{}>", me.user_id),
        ] {
            match client.search_messages(&q, 3).await {
                Ok(found) => {
                    println!(
                        "query {q:?}: total={} shown={}",
                        found.total,
                        found.matches.len()
                    );
                    for m in found.matches.iter().take(2) {
                        println!(
                            "   [{}] {} :: {}",
                            m.channel
                                .as_ref()
                                .map(|c| c.name.clone())
                                .unwrap_or_default(),
                            m.username.clone().unwrap_or_default(),
                            m.text
                                .chars()
                                .take(60)
                                .collect::<String>()
                                .replace('\n', " ")
                        );
                    }
                }
                Err(e) => println!("query {q:?}: {e}"),
            }
        }
        // Does reactions.list show reactions *to* my messages?
        match client
            .get::<Value>("reactions.list", &[("limit", "3".into())])
            .await
        {
            Ok(v) => println!(
                "\nreactions.list: keys={:?}",
                v.as_object().map(|o| o.keys().collect::<Vec<_>>())
            ),
            Err(e) => println!("\nreactions.list: {e}"),
        }
    });
}
