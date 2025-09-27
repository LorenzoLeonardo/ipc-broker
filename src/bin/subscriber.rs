use ipc_broker::rpc::{RpcRequest, RpcResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

async fn subscribe<S>(stream: &mut S)
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // Subscribe to topic "news"
    let sub = RpcRequest::Subscribe {
        topic: "news".into(),
    };
    send_msg(stream, &sub).await;

    println!("Subscribed to 'news' topic");

    let mut buf = vec![0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            println!("Connection closed");
            break;
        }

        if let Ok(resp) = serde_json::from_slice::<RpcResponse>(&buf[..n]) {
            println!("[Subscriber] Received: {resp:?}");
        } else {
            eprintln!(
                "[Subscriber] Unknown message: {}",
                String::from_utf8_lossy(&buf[..n])
            );
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let use_tcp = false; // change to false for Unix socket

    if use_tcp {
        let mut stream = TcpStream::connect("127.0.0.1:5000").await?;
        subscribe(&mut stream).await;
    } else {
        let mut stream = UnixStream::connect("/tmp/ipc_broker.sock").await?;
        subscribe(&mut stream).await;
    }

    Ok(())
}
