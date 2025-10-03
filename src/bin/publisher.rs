use ipc_broker::{client::ClientHandle, logger};
use serde_json::json;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    logger::setup_logger();

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

    log::info!("[Publisher] done broadcasting");
    Ok(())
}
