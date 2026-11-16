//! Test helper for scripts/test-updater-docker.sh: connects to /ws/ui with the certs of a ui device
//! and prints `VERSION <node> <version>` / `STATUS <node> <state json>` lines.
//! Usage: ui_probe <hub https url> <device dir> <seconds> [--update]
//! With `--update` it sends `UpdateNode{node: None}` once the first NodeVersion frame was seen.
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use ulak_proto::{decode, encode, HubToUi, UiToHub};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        return Err("usage: ui_probe <hub url> <device dir> <seconds> [--update]".into());
    }
    let read = |f: &str| std::fs::read_to_string(format!("{}/{f}", a[2]));
    let tls = ulakd::hub::client_tls(&read("ca.pem")?, &read("cert.pem")?, &read("key.pem")?)?;
    let url = a[1].replace("https://", "wss://") + "/ws/ui";
    let mut ws = ulakd::hub::connect(&url, Some(tls)).await?;
    let mut update = a.get(4).is_some_and(|s| s == "--update");
    let end = tokio::time::Instant::now() + Duration::from_secs(a[3].parse()?);
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(end, ws.next()).await {
        let Message::Text(t) = m else { continue };
        match decode::<HubToUi>(&t) {
            Ok(HubToUi::NodeVersion { node, info }) => {
                println!("VERSION {} {}", node.as_str(), info.version());
                if std::mem::take(&mut update) {
                    ws.send(Message::text(encode(&UiToHub::UpdateNode { node: None })?))
                        .await?;
                }
            }
            Ok(HubToUi::UpdateStatus { node, status }) => {
                println!(
                    "STATUS {} {}",
                    node.as_str(),
                    serde_json::to_string(&status)?
                )
            }
            _ => {}
        }
    }
    Ok(())
}
