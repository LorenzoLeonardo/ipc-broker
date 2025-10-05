use async_trait::async_trait;
use ipc_broker::worker::{SharedObject, WorkerBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize, Serialize)]
struct Args {
    a: i32,
    b: i32,
}

pub struct Calculator;
#[async_trait]
impl SharedObject for Calculator {
    async fn call(&self, method: &str, args: &Value) -> Value {
        let parsed = serde_json::from_value(args.clone());

        match (method, parsed) {
            ("add", Ok(Args { a, b })) => json!({"sum": (a + b)}),
            ("mul", Ok(Args { a, b })) => json!({"product": (a * b)}),
            (_, Err(e)) => Value::String(format!("Invalid args: {e}")),
            _ => Value::String("Unknown method".into()),
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let calc = Calculator;

    WorkerBuilder::new().add("math", calc).spawn().await?;
    Ok(())
}
