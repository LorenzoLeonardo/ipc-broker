use async_trait::async_trait;
use ipc_broker::worker::{SharedObject, run_worker};
use serde_json::Value;

pub struct Calculator;

#[async_trait]
impl SharedObject for Calculator {
    fn name(&self) -> &str {
        "Calculator"
    }

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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let calc = Calculator;

    run_worker(calc).await?;

    Ok(())
}
