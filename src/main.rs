use ipc_broker::broker;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");
    println!("{name} has started v{version}...");

    broker::run_broker().await
}
