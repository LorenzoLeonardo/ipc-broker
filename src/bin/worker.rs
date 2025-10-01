use async_trait::async_trait;
use ipc_broker::worker::{SharedObject, WorkerBuilder};
use serde_json::Value;

pub struct Calculator;

#[async_trait]
impl SharedObject for Calculator {
    async fn call(&self, method: &str, args: &Value) -> Value {
        match method {
            "add" => {
                let a = args.get(0).and_then(Value::as_i64).unwrap_or(0);
                let b = args.get(1).and_then(Value::as_i64).unwrap_or(0);
                (a + b).into()
            }
            "mul" => {
                let a = args.get(0).and_then(Value::as_i64).unwrap_or(0);
                let b = args.get(1).and_then(Value::as_i64).unwrap_or(0);
                (a * b).into()
            }
            _ => Value::String("Unknown method".into()),
        }
    }
}

struct Logger;
#[async_trait]
impl SharedObject for Logger {
    async fn call(&self, method: &str, args: &Value) -> Value {
        println!("LOG: {method} -> {args}");
        Value::Null
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let calc = Calculator;
    let logger = Logger;

    WorkerBuilder::new()
        .add("Calculator", calc)
        .add("Logger", logger)
        .spawn()
        .await?;

    Ok(())
}
