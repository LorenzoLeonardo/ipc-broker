use ipc_broker::rpc::RpcRequest;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UnixStream};

// generic send
async fn send_msg<T, S>(stream: &mut S, msg: &T)
where
    T: serde::Serialize,
    S: AsyncWriteExt + Unpin,
{
    let bytes = serde_json::to_vec(msg).unwrap();
    stream.write_all(&bytes).await.unwrap();
}

async fn publish<S>(stream: &mut S)
where
    S: AsyncWriteExt + Unpin,
{
    let event = RpcRequest::Publish {
        topic: "news".into(),
        payload: json!({"headline": "Rust broker eventing works!"}).to_string(),
    };

    send_msg(stream, &event).await;
    println!("[Publisher] Published event to 'news'");
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let use_tcp = false; // change to false for Unix socket

    if use_tcp {
        let mut stream = TcpStream::connect("127.0.0.1:5000").await?;
        publish(&mut stream).await;
    } else {
        let mut stream = UnixStream::connect("/tmp/ipc_broker.sock").await?;
        publish(&mut stream).await;
    }

    Ok(())
}
