use ipc_broker::client::ClientHandle;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let client = ClientHandle::connect().await?;

    client
        .subscribe_async("news", |value| {
            println!("[News] Received: {value:?}");
        })
        .await;

    client
        .subscribe_async("news1", |value| {
            println!("[News1] Received: {value:?}");
        })
        .await;

    // Keep the main task alive
    tokio::signal::ctrl_c().await?;
    Ok(())
}
