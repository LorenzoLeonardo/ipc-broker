use async_trait::async_trait;
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UnixStream},
    sync::oneshot,
};

use crate::{
    client::BUF_SIZE,
    rpc::{RpcRequest, RpcResponse},
};

trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

/// Trait for a worker object that can handle RPC calls
#[async_trait]
pub trait SharedObject: Send + Sync {
    fn name(&self) -> &str;

    async fn call(&self, method: &str, args: &Value) -> Value;
}

/// Runs a worker that registers a SharedObject with the broker
pub async fn run_worker(
    obj: impl SharedObject + 'static,
    ready_tx: Option<oneshot::Sender<()>>,
) -> std::io::Result<()> {
    let mut stream: Box<dyn AsyncStream + Send + Unpin> =
        if let Ok(ip) = std::env::var("BROKER_ADDR") {
            let tcp = TcpStream::connect(ip.as_str()).await?;
            println!("Client connected via TCP");
            Box::new(tcp)
        } else {
            let unix = UnixStream::connect("/tmp/ipc_broker.sock").await?;
            println!("Client connected via Unix socket");
            Box::new(unix)
        };
    if let Some(tx) = ready_tx {
        let _ = tx.send(()); // signal bound & ready
    }
    // Register object
    let reg = RpcRequest::RegisterObject {
        object_name: obj.name().into(),
    };
    stream.write_all(&serde_json::to_vec(&reg).unwrap()).await?;

    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        if let Ok(req) = serde_json::from_slice::<RpcRequest>(&buf[..n]) {
            match req {
                RpcRequest::Call {
                    call_id,
                    object_name,
                    method,
                    args,
                } => {
                    println!("Worker handling {method}({args})");
                    let result = obj.call(&method, &args).await;

                    let resp = RpcResponse::Result {
                        call_id,
                        object_name,
                        value: result,
                    };

                    let resp_bytes = serde_json::to_vec(&resp).unwrap();
                    stream.write_all(&resp_bytes).await?;
                }
                _ => {
                    println!("Worker got unsupported request: {req:?}");
                }
            }
        } else if let Ok(resp) = serde_json::from_slice::<RpcResponse>(&buf[..n]) {
            println!("Worker got response: {resp:?}");
        } else {
            eprintln!("Unknown message: {}", String::from_utf8_lossy(&buf[..n]));
        }
    }

    Ok(())
}
