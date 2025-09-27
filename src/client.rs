use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UnixStream},
    sync::{mpsc, oneshot},
};
use uuid::Uuid;

use crate::rpc::{CallId, RpcRequest, RpcResponse};
pub trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

pub const BUF_SIZE: usize = (u16::MAX as usize) + 1;

/// Request from a handle to the client actor
struct ClientMsg {
    req: RpcRequest,
    resp_tx: oneshot::Sender<std::io::Result<RpcResponse>>,
}

#[derive(Clone)]
pub struct ClientHandle {
    tx: mpsc::Sender<ClientMsg>,
}

impl ClientHandle {
    pub async fn connect() -> std::io::Result<Self> {
        // Pick transport
        let stream: Box<dyn AsyncStream + Send + Unpin> =
            if let Ok(ip) = std::env::var("BROKER_ADDR") {
                let tcp = TcpStream::connect(ip.as_str()).await?;
                println!("Client connected via TCP");
                Box::new(tcp)
            } else {
                let unix = UnixStream::connect("/tmp/ipc_broker.sock").await?;
                println!("Client connected via Unix socket");
                Box::new(unix)
            };

        // channel for handles -> actor
        let (tx, mut rx) = mpsc::channel::<ClientMsg>(32);

        // Spawn the client actor task
        tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = vec![0u8; BUF_SIZE];

            while let Some(ClientMsg { req, resp_tx }) = rx.recv().await {
                // Serialize request
                let data = match serde_json::to_vec(&req) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = resp_tx
                            .send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)));
                        continue;
                    }
                };

                // Send to server
                if let Err(e) = stream.write_all(&data).await {
                    let _ = resp_tx.send(Err(e));
                    continue;
                }

                // Wait for response
                match stream.read(&mut buf).await {
                    Ok(0) => {
                        let _ = resp_tx.send(Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "Connection closed by server",
                        )));
                        break;
                    }
                    Ok(n) => {
                        let resp: Result<RpcResponse, _> = serde_json::from_slice(&buf[..n]);
                        match resp {
                            Ok(r) => {
                                let _ = resp_tx.send(Ok(r));
                            }
                            Err(e) => {
                                let _ = resp_tx.send(Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    e,
                                )));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = resp_tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        Ok(Self { tx })
    }

    pub async fn remote_call(
        &self,
        object: &str,
        method: &str,
        args: &serde_json::Value,
    ) -> std::io::Result<RpcResponse> {
        let call_id = CallId(Uuid::new_v4().as_u128());

        let req = RpcRequest::Call {
            call_id,
            object_name: object.into(),
            method: method.into(),
            args: args.clone(),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let msg = ClientMsg { req, resp_tx };

        // Send request to actor
        self.tx
            .send(msg)
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Actor dropped"))?;

        // Wait for reply
        resp_rx.await.unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "Actor task ended",
            ))
        })
    }

    pub async fn publish(&self, topic: &str, args: &serde_json::Value) -> std::io::Result<()> {
        let (resp_tx, _resp_rx) = oneshot::channel();
        let msg = ClientMsg {
            req: RpcRequest::Publish {
                topic: topic.into(),
                args: args.clone(),
            },
            resp_tx,
        };

        self.tx
            .send(msg)
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Actor dropped"))
    }
}
