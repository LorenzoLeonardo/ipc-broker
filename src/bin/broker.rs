use ipc_broker::broker;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    broker::run_broker().await
}
