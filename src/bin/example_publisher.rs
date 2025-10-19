use ipc_broker::client::IPCClient;
use serde_json::Value;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let proxy = IPCClient::connect().await?;

    proxy
        .publish("sensor", "temperature", &Value::String("25.6°C".into()))
        .await?;

    println!("[Publisher] done broadcasting");
    Ok(())
}
