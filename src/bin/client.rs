use ipc_broker::client::ClientHandle;
use serde_json::json;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let proxy = ClientHandle::connect().await?;

    let response = proxy
        .remote_call("Calculator", "add", &json!([5, 7]))
        .await?;

    println!("Client got response: {response:?}");

    let response = proxy
        .remote_call("Calculator", "mul", &json!([5, 7]))
        .await?;

    println!("Client got response: {response:?}");

    Ok(())
}
