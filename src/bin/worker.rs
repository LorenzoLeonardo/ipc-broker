use ipc_broker::client::BUF_SIZE;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};

use ipc_broker::rpc::{RpcRequest, RpcResponse};

trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // --- pick transport ---
    // Example: switch between TCP and Unix socket
    let use_tcp = false; // change to false to test unix socket

    let mut stream: Box<dyn AsyncStream + Send + Unpin> = if use_tcp {
        let tcp = TcpStream::connect("127.0.0.1:5000").await?;
        println!("Worker connected via TCP");
        Box::new(tcp)
    } else {
        let unix = UnixStream::connect("/tmp/ipc_broker.sock").await?;
        println!("Worker connected via Unix socket");
        Box::new(unix)
    };

    // Register Calculator object
    let reg = RpcRequest::RegisterObject {
        object_name: "Calculator".into(),
    };
    stream.write_all(&serde_json::to_vec(&reg).unwrap()).await?;

    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        println!("{n} bytes read");
        println!("{} ", String::from_utf8_lossy(&buf[..n]));

        if let Ok(req) = serde_json::from_slice::<RpcRequest>(&buf[..n]) {
            match req {
                RpcRequest::RegisterObject { object_name } => {
                    unimplemented!("{object_name}");
                }
                RpcRequest::Call {
                    call_id,
                    object_name,
                    method,
                    args,
                } => {
                    println!("Worker got Call for {object_name}.{method}({args:?})");
                    let resp = match method.as_str() {
                        "add" => RpcResponse::Result {
                            call_id,
                            value: (args.get(0).and_then(Value::as_i64).unwrap()
                                + args.get(1).and_then(Value::as_i64).unwrap())
                                as i32,
                        },
                        "mul" => RpcResponse::Result {
                            call_id,
                            value: (args.get(0).and_then(Value::as_i64).unwrap()
                                * args.get(1).and_then(Value::as_i64).unwrap())
                                as i32,
                        },
                        _ => RpcResponse::Error {
                            call_id: Some(call_id),
                            message: "Unknown method".into(),
                        },
                    };

                    let resp_bytes = serde_json::to_vec(&resp).unwrap();
                    stream.write_all(&resp_bytes).await?;
                }
                RpcRequest::Subscribe { topic } => {
                    unimplemented!("{topic}");
                }
                RpcRequest::Publish { topic, args } => {
                    unimplemented!("{topic} {args}");
                }
            }
        } else if let Ok(resp) = serde_json::from_slice::<RpcResponse>(&buf[..n]) {
            println!("Got RpcResponse: {resp:?}");
        } else {
            eprintln!("Unknown message: {}", String::from_utf8_lossy(&buf[..n]));
        }
    }

    Ok(())
}
