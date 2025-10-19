use ipc_broker::client::IPCClient;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let client = IPCClient::connect().await?;

    client
        .subscribe_async("sensor", "temperature", |value| {
            println!("[News] Received: {value:?}");
        })
        .await;

    tokio::signal::ctrl_c().await?;
    Ok(())
}
