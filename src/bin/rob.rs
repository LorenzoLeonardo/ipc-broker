use ipc_broker::client::ClientHandle;
use serde_json::Value;

fn format_value(value: &Value, indent: usize) -> String {
    let padding = " ".repeat(indent);
    match value {
        Value::Null => format!("{padding}Null"),
        Value::Bool(b) => format!("{padding}{b}"),
        Value::Number(n) => format!("{padding}{n}"),
        Value::String(s) => format!("{padding}\"{s}\""),
        Value::Array(arr) => {
            let mut out = format!("{padding}List:\n");
            for (i, v) in arr.iter().enumerate() {
                out.push_str(&format!(
                    "{padding}  {i} : :{}\n",
                    format_value(v, indent + 1)
                ));
            }
            out
        }
        Value::Object(obj) => {
            let mut out = format!("{padding}Map:\n");
            for (k, v) in obj {
                out.push_str(&format!("{padding}  \"{k}\":{}\n", format_value(v, indent)));
            }
            out
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let proxy = ClientHandle::connect().await?;

    let response = proxy
        .remote_call::<Value, Value>("rob", "listObjects", Value::Null)
        .await?;

    println!("Result:\n  {}", format_value(&response, 0));
    Ok(())
}
