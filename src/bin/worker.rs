use async_trait::async_trait;
use ipc_broker::worker::{SharedObject, WorkerBuilder};
use serde::{Deserialize, Serialize};
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

#[derive(Serialize, Deserialize, Debug)]
struct Param {
    name: String,
    age: u32,
    phone: String,
}

impl From<Param> for Value {
    fn from(val: Param) -> Self {
        serde_json::to_value(val).unwrap()
    }
}

struct TestObject;
#[async_trait]
impl SharedObject for TestObject {
    async fn call(&self, method: &str, args: &Value) -> Value {
        let param: Param = serde_json::from_value(args.clone()).unwrap();
        println!(
            "TestObject: {method} -> {args} -> Name: {} age: {} phone: {}",
            param.name, param.age, param.phone
        );
        param.into()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let calc = Calculator;
    let logger = Logger;
    let test_obj = TestObject;

    WorkerBuilder::new()
        .add("Calculator", calc)
        .add("Logger", logger)
        .add("TestObject", test_obj)
        .spawn()
        .await?;

    Ok(())
}
