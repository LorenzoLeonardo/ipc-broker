use ipc_broker::client::ClientHandle;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, Debug)]
struct Param {
    name: String,
    age: u32,
    phone: String,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let proxy = ClientHandle::connect().await?;

    proxy.wait_for_object("Calculator").await?;

    let response = proxy
        .remote_call::<Value, i32>("Calculator", "add", json!([5, 7]))
        .await?;

    println!("Client got response: {response:?}");

    let response = proxy
        .remote_call::<Value, i32>("Calculator", "mul", json!([5, 7]))
        .await?;

    println!("Client got response: {response:?}");

    let response = proxy
        .remote_call::<Value, Value>("Logger", "log", Value::Null)
        .await?;

    println!("Client got response: {response:?}");

    let response = proxy
        .remote_call::<Param, Param>(
            "TestObject",
            "mul",
            Param {
                name: "Leonardo".into(),
                age: 30,
                phone: "123456789".into(),
            },
        )
        .await?;

    println!("Client got response: {response:?}");

    Ok(())
}
