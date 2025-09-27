use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use uuid::Uuid;

use ipc_broker::rpc::{CallId, RpcRequest, RpcResponse};

trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    let use_tcp = false; // change to false to use unix socket

    let mut stream: Box<dyn AsyncStream + Send + Unpin> = if use_tcp {
        let tcp = TcpStream::connect("127.0.0.1:5000").await?;
        println!("Client connected via TCP");
        Box::new(tcp)
    } else {
        let unix = UnixStream::connect("/tmp/ipc_broker.sock").await?;
        println!("Client connected via Unix socket");
        Box::new(unix)
    };

    let call_id = CallId(Uuid::new_v4().as_u128());

    // Call Calculator.mul(5, 7)
    let req = RpcRequest::Call {
        call_id,
        object_name: "Calculator".into(),
        method: "mul".into(),
        args: vec![5, 7],
    };
    stream.write_all(&serde_json::to_vec(&req).unwrap()).await?;

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        eprintln!("Connection closed by server");
        return Ok(());
    }

    let resp: RpcResponse = serde_json::from_slice(&buf[..n]).unwrap();
    println!("Client got response: {resp:?}");

    Ok(())
}
