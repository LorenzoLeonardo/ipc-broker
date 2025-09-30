use async_trait::async_trait;
use serde_json::{Deserializer, Value};
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
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
            #[cfg(unix)]
            {
                let unix = UnixStream::connect("/tmp/ipc_broker.sock").await?;
                println!("Client connected via Unix socket");
                Box::new(unix)
            }

            #[cfg(windows)]
            {
                loop {
                    let pipe_name = r"\\.\pipe\ipc_broker";
                    let res = match ClientOptions::new().open(pipe_name) {
                        Ok(pipe) => Box::new(pipe),
                        Err(e) if e.raw_os_error() == Some(231) => {
                            // All pipe instances busy → wait and retry

                            use std::time::Duration;

                            eprintln!("All pipe instances busy, retrying...");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                        Err(e) => panic!("Failed to connect to pipe: {}", e),
                    };
                    println!("Client connected via Named Pipe: {pipe_name}");
                    break res;
                }
            }
        };

    if let Some(tx) = ready_tx {
        let _ = tx.send(()); // signal bound & ready
    }

    // Register object
    let reg = RpcRequest::RegisterObject {
        object_name: obj.name().into(),
    };
    stream.write_all(&serde_json::to_vec(&reg).unwrap()).await?;

    let mut buf = Vec::new();
    let mut tmp = vec![0u8; BUF_SIZE];

    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);

        // keep extracting JSON messages until nothing left to parse
        loop {
            let mut de = Deserializer::from_slice(&buf).into_iter::<serde_json::Value>();

            match de.next() {
                Some(Ok(val)) => {
                    let consumed = de.byte_offset();

                    if let Ok(req) = serde_json::from_value::<RpcRequest>(val.clone()) {
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
                    } else if let Ok(resp) = serde_json::from_value::<RpcResponse>(val) {
                        println!("Worker got response: {resp:?}");
                    }

                    // remove parsed bytes and keep looping
                    buf.drain(..consumed);
                }
                Some(Err(e)) if e.is_eof() => {
                    // partial JSON, wait for more data
                    break;
                }
                Some(Err(e)) => {
                    eprintln!("JSON parse error: {e:?}");
                    // skip bad data
                    buf.clear();
                    break;
                }
                None => break,
            }
        }
    }

    Ok(())
}
