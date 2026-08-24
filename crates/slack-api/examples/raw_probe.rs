use serde_json::Value;
use slack_api::{ALL_CONVERSATION_TYPES, SlackClient, store};

fn main() {
    let (token, _) = store::load().unwrap().unwrap();
    let client = SlackClient::new(token).unwrap();
    futures::executor::block_on(async {
        let convs = client
            .list_conversations(ALL_CONVERSATION_TYPES)
            .await
            .unwrap();
        let mut looked = 0;
        for c in convs
            .iter()
            .filter(|c| c.name.contains("cli-skill") || c.name.contains("看板"))
        {
            let Ok(v) = client
                .get::<Value>(
                    "conversations.history",
                    &[("channel", c.id.clone()), ("limit", "60".into())],
                )
                .await
            else {
                continue;
            };
            for m in v["messages"].as_array().unwrap_or(&vec![]) {
                for f in m["files"].as_array().unwrap_or(&vec![]) {
                    looked += 1;
                    let mut keys: Vec<&String> = f
                        .as_object()
                        .map(|o| o.keys().collect())
                        .unwrap_or_default();
                    keys.sort();
                    println!("mimetype={} filetype={}", f["mimetype"], f["filetype"]);
                    println!(
                        "  has thumb_360={} thumb_720={} thumb_video={} mp4={} url_private={}",
                        !f["thumb_360"].is_null(),
                        !f["thumb_720"].is_null(),
                        !f["thumb_video"].is_null(),
                        !f["mp4"].is_null(),
                        !f["url_private"].is_null()
                    );
                    println!(
                        "  keys={:?}",
                        keys.iter().map(|k| k.as_str()).collect::<Vec<_>>()
                    );
                    if looked >= 4 {
                        return;
                    }
                }
            }
        }
        println!("files seen: {looked}");
    });
}
