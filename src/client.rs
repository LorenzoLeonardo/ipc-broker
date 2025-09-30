use std::sync::Arc;

use serde_json::Value;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use uuid::Uuid;

use crate::rpc::{CallId, RpcRequest, RpcResponse};
pub trait AsyncStream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + Unpin> AsyncStream for T {}

pub const BUF_SIZE: usize = (u16::MAX as usize) + 1;
pub const CHANNEL_SIZE: usize = 1024;

/// Request from a handle to the client actor
enum ClientMsg {
    Request {
        req: RpcRequest,
        resp_tx: oneshot::Sender<std::io::Result<RpcResponse>>,
    },
    Subscribe {
        topic: String,
        updates: mpsc::UnboundedSender<serde_json::Value>,
    },
}

#[derive(Clone)]
pub struct ClientHandle {
    tx: mpsc::UnboundedSender<ClientMsg>,
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
                // Local IPC depending on OS
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
                        break res;
                    }
                }
            };

        // channel for handles -> actor
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();

        // Spawn the client actor task
        tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = vec![0u8; BUF_SIZE];
            let mut subs: std::collections::HashMap<
                String,
                Vec<mpsc::UnboundedSender<serde_json::Value>>,
            > = std::collections::HashMap::new();
            let mut leftover = Vec::new();
            loop {
                tokio::select! {
                    Some(msg) = rx.recv() => {
                        match msg {
                            ClientMsg::Request { req, resp_tx } => {
                                // Serialize and send
                                let data = match serde_json::to_vec(&req) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        let _ = resp_tx.send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)));
                                        continue;
                                    }
                                };
                                if let Err(e) = stream.write_all(&data).await {
                                    let _ = resp_tx.send(Err(e));
                                    continue;
                                }

                                // Wait for one response
                                match req {
                                    RpcRequest::Call { .. } | RpcRequest::RegisterObject { .. } => {
                                        // Only these expect a response
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
                                                    Ok(r) => { let _ = resp_tx.send(Ok(r)); }
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
                                    RpcRequest::Publish { .. } | RpcRequest::Subscribe { .. } => {
                                        // Fire-and-forget: do not await a response
                                        let _ = resp_tx.send(Ok(RpcResponse::Event {
                                            topic: "".into(),
                                            args: serde_json::Value::Null,
                                        }));
                                    }
                                }
                            }
                            ClientMsg::Subscribe { topic, updates } => {
                                println!("Client subscribing to topic: {topic}");
                                subs.entry(topic.clone())
                                    .or_default()
                                    .push(updates);
                                let _ = stream.write_all(
                                    &serde_json::to_vec(&RpcRequest::Subscribe { topic }).unwrap()
                                ).await;
                            }

                        }
                    }
                    Ok(n) = stream.read(&mut buf) => {
                        if n == 0 {
                            break;
                        }
                        println!("Data {}", String::from_utf8_lossy(&buf[..n]));
                        // Append new data to leftover
                        leftover.extend_from_slice(&buf[..n]);
                        let mut slice = leftover.as_slice();

                        while !slice.is_empty() {
                            let mut de = serde_json::Deserializer::from_slice(slice).into_iter::<RpcResponse>();
                            match de.next() {
                                Some(Ok(resp)) => {
                                    let consumed = de.byte_offset();
                                    slice = &slice[consumed..];
                                    println!("Chunk {resp:?}");
                                    // Handle Publish notifications
                                    if let RpcResponse::Event { topic, args } = resp {
                                        if let Some(subscribers) = subs.get(&topic) {
                                            for tx in subscribers {
                                                let _ = tx.send(args.clone());
                                            }
                                    }
                                    } else {
                                        // Other responses are ignored here; handled elsewhere
                                    }
                                }
                                Some(Err(_)) => {
                                    // Partial JSON, wait for more bytes
                                    break;
                                }
                                None => break,
                            }
                        }

                        // Keep unconsumed bytes for next read
                        leftover = slice.to_vec();
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
        let call_id = CallId(Uuid::new_v4().to_string());

        let req = RpcRequest::Call {
            call_id,
            object_name: object.into(),
            method: method.into(),
            args: args.clone(),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let msg = ClientMsg::Request { req, resp_tx };

        // Send request to actor
        self.tx
            .send(msg)
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
        let msg = ClientMsg::Request {
            req: RpcRequest::Publish {
                topic: topic.into(),
                args: args.clone(),
            },
            resp_tx,
        };

        self.tx
            .send(msg)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Actor dropped"))
    }

    pub async fn subscribe(&self, topic: &str) -> mpsc::UnboundedReceiver<serde_json::Value> {
        let (tx_updates, rx_updates) = mpsc::unbounded_channel();
        let _ = self.tx.send(ClientMsg::Subscribe {
            topic: topic.into(),
            updates: tx_updates,
        });
        rx_updates
    }

    pub async fn subscribe_async<F>(&self, topic: &str, callback: F)
    where
        F: Fn(Value) + Send + Sync + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let callback = Arc::new(callback);

        // Tell the actor to subscribe
        let _ = self.tx.send(ClientMsg::Subscribe {
            topic: topic.into(),
            updates: tx,
        });

        // Spawn a task to handle messages and call the callback synchronously
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let cb = callback.clone();
                // Call synchronously
                cb(msg);
            }
        });
    }
}
