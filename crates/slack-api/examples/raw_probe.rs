use slack_api::{SlackClient, store};

fn main() {
    let (token, _) = store::load().unwrap().unwrap();
    let client = SlackClient::new(token).unwrap();
    futures::executor::block_on(async {
        for id in ["U0B5TB3BH2P", "U0929ECTE94", "U092MCQ1WTA", "U0B67DAJCP9"] {
            match client.user_info(id).await {
                Ok(u) => println!(
                    "{id}: OK name={:?} deleted={} bot={}",
                    u.display_name(),
                    u.deleted,
                    u.is_bot
                ),
                Err(e) => println!("{id}: {e}"),
            }
        }
    });
}
