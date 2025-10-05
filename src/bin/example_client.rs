use ipc_broker::client::ClientHandle;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug)]
struct Param {
    a: i32,
    b: i32,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let proxy = ClientHandle::connect().await?;

    let response = proxy
        .remote_call::<Param, Value>("math", "add", Param { a: 10, b: 32 })
        .await?;

    println!("Client got response: {response}");

    Ok(())
}
