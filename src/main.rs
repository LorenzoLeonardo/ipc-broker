use std::io;

use chrono::Local;
use fern::Dispatch;
use ipc_broker::broker;
use log::LevelFilter;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    setup_logger();
    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");
    log::info!("{name} has started v{version}...");

    broker::run_broker().await
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

fn setup_logger() {
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
