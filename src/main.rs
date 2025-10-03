use ipc_broker::{broker, logger};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    logger::setup_logger();

    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");
    log::info!("{name} has started v{version}...");

    broker::run_broker().await
}
