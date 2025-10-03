use ipc_broker::{client::ClientHandle, logger};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    logger::setup_logger();

    let client = ClientHandle::connect().await?;

    client
        .subscribe_async("object_name", "news", |value| {
            log::info!("[News] Received: {value:?}");
        })
        .await;

    client
        .subscribe_async("object_name", "news1", |value| {
            log::info!("[News1] Received: {value:?}");
        })
        .await;

    // Keep the main task alive
    tokio::signal::ctrl_c().await?;
    Ok(())
}
