use std::io;

use async_trait::async_trait;
use chrono::Local;
use fern::Dispatch;
use ipc_broker::worker::{SharedObject, WorkerBuilder};
use log::LevelFilter;
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

fn logging_level() -> LevelFilter {
    match std::env::var("BROKER_DEBUG").as_deref() {
        Ok("trace") => LevelFilter::Trace,
        Ok("debug") => LevelFilter::Debug,
        Ok("info") => LevelFilter::Info,
        Ok("warn") => LevelFilter::Warn,
        Ok("error") => LevelFilter::Error,
        _ => LevelFilter::Info, // default if unset or unknown
    }
}

pub fn setup_logger() {
    let level_filter = logging_level();

    if let Err(e) = Dispatch::new()
        .format(move |out, message, record| {
            let file = record.file().unwrap_or("unknown_file");
            let line = record.line().map_or(0, |l| l);

            match level_filter {
                LevelFilter::Off
                | LevelFilter::Error
                | LevelFilter::Warn
                | LevelFilter::Debug
                | LevelFilter::Trace => {
                    out.finish(format_args!(
                        "[{}][{}]: {} <{}:{}>",
                        Local::now().format("%b-%d-%Y %H:%M:%S.%f"),
                        record.level(),
                        message,
                        file,
                        line,
                    ));
                }
                LevelFilter::Info => {
                    out.finish(format_args!(
                        "[{}]: {} <{}:{}>",
                        record.level(),
                        message,
                        file,
                        line,
                    ));
                }
            }
        })
        .level(level_filter)
        .chain(io::stdout())
        .apply()
    {
        log::error!("Logger initialization failed: {e}");
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    setup_logger();
    let calc = Calculator;

    let (builder, shutdown) = WorkerBuilder::new()
        .add("math", calc)
        .with_graceful_shutdown();

    let handle = tokio::spawn(async move { builder.spawn().await });

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let _ = shutdown.send(true);
        log::info!("Sending true . . .")
    });

    handle.await??;
    Ok(())
}
