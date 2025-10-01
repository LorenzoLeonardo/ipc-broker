use ipc_broker::client::ClientHandle;
use serde_json::json;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let proxy = ClientHandle::connect().await?;

    proxy
        .publish(
            "object_name",
            "news",
            &json!({"headline": "Rust broker eventing works!"}),
        )
        .await?;

    proxy
        .publish(
            "object_name",
            "news1",
            &json!({"headline": "Another news!"}),
        )
        .await?;

    println!("[Publisher] done broadcasting");
    Ok(())
}
